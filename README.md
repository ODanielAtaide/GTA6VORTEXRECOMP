# Vortex

Experimental Windows compatibility research. This repository contains original diagnostic tooling and references an independently available public upstream compatibility project as a pinned Git submodule. Upstream retains its own copyright and GPL-3.0-or-later license. No proprietary game files or keys are included.

## Source baseline

- Public upstream: https://github.com/fxceless/victory-gta6
- Pinned revision: 16849898f3282c4a7189b1557289c194bc720052
- Upstream is experimental; commercial-game playability has not been verified by Vortex.

## Development

Clone with `git clone --recurse-submodules https://github.com/ODanielAtaide/GTA6VORTEXRECOMP.git`.

Run Vortex's original tooling tests: `python -m unittest discover -v`.

On a Windows development machine with the pinned Rust toolchain installed, run the public upstream's synthetic tests from `vendor/victory`: `cargo test --workspace --locked`. No game data is needed for these tests. Review upstream code before running it.

## Diagnostics

`python vortex_metrics.py path/to/benchmark.log` summarizes periodic frame-rate samples from the public upstream logging format. These are window averages, not individual frame times or 1% lows. Compare captures from equivalent authorized workloads and identical cache conditions.

## Status

No Vortex runtime fixes or commercial-game compatibility claims have been validated. This is an engineering baseline, not a playable release. No leaked or removed repository content is hosted here. Not affiliated with Rockstar Games, Take-Two or Microsoft.

# Editable upstream source

The import workflow fetches the public VIctory revision `16849898f3282c4a7189b1557289c194bc720052`, verifies each copied Git blob and creates an editable snapshot in `src-victory/`. It does not execute upstream code. Existing Vortex files and the pinned baseline submodule remain unchanged.

Upstream source: https://github.com/fxceless/victory-gta6

The upstream license is GPL-3.0-or-later; original notices and the LICENSE are preserved. The upstream asset policy explicitly excludes `crates/victory-formats/src/bcpack-tables.bin` from that license. The snapshot intentionally omits that file and is not a complete buildable distribution. Do not add material without appropriate redistribution rights. This workflow is not intended to recover blocked or removed repositories; if the public source becomes unavailable, the import must fail.

After a successful import, edit the Rust source under `src-victory/crates/` and record changes separately. The source snapshot is imported only once to avoid overwriting edits. Build and test status of the separately pinned baseline is independent of the snapshot. No commercial-game compatibility has been established.

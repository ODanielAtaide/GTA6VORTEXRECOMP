# Vortex changes

## PE directory validation

Hardened the read-only PE32+ data-directory offset helper to reject invalid DOS/PE signatures, wrong optional-header magic, truncated declared headers and directory entries extending beyond SizeOfOptionalHeader. Added two synthetic regression tests. This is a correctness and robustness fix, not a rendering optimization.

The editable snapshot has its own Windows CI workflow, separate from the pinned baseline. No game data is required or accepted by CI. Runtime compatibility and graphics performance remain unverified.

## TLS callback parsing

Reject undersized/truncated TLS directories, callback arrays without a null terminator, oversized arrays and callback addresses outside the declared image. Added three synthetic tests. This is defensive PE parsing, not a graphics or performance change.

## Vortex branding and optional performance profile

Public CLI package and executable renamed to `vortex`; window branding renamed to Vortex. Internal crates and existing environment/configuration interfaces remain stable. Credits and license retained. Added an opt-in ThinLTO CPU build profile and 30 FPS window-budget telemetry. Neither implies a measured runtime speedup.

## Packed linear texture copy

For linear subresources with equal, tightly packed source and destination row pitches, coalesce all active rows into one copy per depth slice. Padded and tiled layouts use the original path. Added three synthetic regression tests. This reduces copy invocations for the eligible case; a frame-rate benefit is not yet measured.

## Bounded shader fingerprinting and concurrent cache publication

Stream the executable fingerprint in 64 KiB chunks instead of loading an extra copy of the executable. Preserve the existing key inputs and order. Handle concurrent publication of the same content-addressed cache entry without reporting a false failure when another writer has already published it. Added synthetic hash-equivalence and concurrent-publication tests. This targets transient memory use and cache robustness, not verified rendering throughput.

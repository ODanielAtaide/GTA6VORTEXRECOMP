# Vortex changes

## PE directory validation

Hardened the read-only PE32+ data-directory offset helper to reject invalid DOS/PE signatures, wrong optional-header magic, truncated declared headers and directory entries extending beyond SizeOfOptionalHeader. Added two synthetic regression tests. This is a correctness and robustness fix, not a rendering optimization.

The editable snapshot has its own Windows CI workflow, separate from the pinned baseline. No game data is required or accepted by CI. Runtime compatibility and graphics performance remain unverified.

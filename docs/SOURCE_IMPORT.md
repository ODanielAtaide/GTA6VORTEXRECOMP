# Source integration

The root workspace incorporates the public VIctory main revision `90228a7dc66010281fdcfad10ac00c3a252be2f3`. All 169 upstream paths are represented at their original paths. The root README, ignore rules, and asset policy contain Vortex-specific additions; the PE parser retains our two earlier patches. The two newer upstream commits are also integrated in the shader IR, pipeline host and translation tool.

Upstream: https://github.com/fxceless/victory-gta6

The merge-history workflow fetches the original full main ancestry and creates a merge commit preserving the integrated tree and previous Vortex commits. It does not force-push or modify the imported tree. History is verified by ancestry checks and must be confirmed by a successful workflow result.

Original source notices and GPL-3.0-or-later license are retained. The BCPack binary is separately documented and is not GPL-licensed. No title assets or keys are included. The old submodule and nested source folder are superseded by the root workspace. Compilation and graphics correctness must be tested separately.

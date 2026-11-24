# Engineering roadmap

The target is correct and stable rendering at 30 FPS (33.33 ms/frame). No delivery date or guaranteed FPS is implied.

| Phase | State | Acceptance |
|---|---|---|
| 1. Public baseline and build | Initial milestone complete | Root workspace, upstream ancestry, reproducible Windows build |
| 2. Runtime correctness and synthetic regression tests | In progress | Validated bounds, deterministic cache handling, equivalent output |
| 3. Profiling and bottleneck isolation | In progress | Repeatable captures separating CPU preparation, GPU idle time, transfers, compilation, RAM and VRAM |
| 4. Texture and memory scheduling | In progress | Bounded queue work, preserved priorities and memory coverage, no visual regressions |
| 5. Graphics pipeline and shader correctness | Open | Required pipelines ready, accurate graphics output, no dropped-work shortcuts |
| 6. Synchronization and transfers | Open | Reduced blocking and redundant transfers, correct resource lifetimes |
| 7. Hardware validation | Open | Repeatable results across supported AMD, NVIDIA and Intel hardware |
| 8. Performance targets | Not achieved | Intermediate research checkpoints 5 and 15 FPS; final 30 FPS with rendering correctness |

## How to help

Pick a narrowly scoped issue and supply a freely shareable synthetic reproducer. Changes should preserve correctness and include evidence. See [CONTRIBUTING.md](../CONTRIBUTING.md) and [development status](DEVELOPMENT_STATUS.md).

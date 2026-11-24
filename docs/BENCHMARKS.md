# Reproducible benchmark comparison

Use original or otherwise authorized workloads only. Keep scene, machine, driver, resolution, build profile, cache state and sampling duration constant. Capture at least five valid windows per run, repeat runs, and review visual output independently.

`python vortex_compare.py baseline.log candidate.log --min-samples 5`

The report shows median frame-window FPS change, frame-time change and pipeline-drop change. A faster result with missing pipeline draws is not an accepted optimization. A passing drop guard is not proof of complete rendering. Missing drop data is reported as unknown, not zero. This tool does not launch or modify any executable.

The Runtime baseline CI checks compilation and synthetic tests of the pinned public upstream on Windows; hosted CI does not establish real-GPU performance.

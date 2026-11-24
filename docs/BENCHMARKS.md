# Reproducible benchmark comparison

Use original or otherwise authorized workloads only. Keep scene, machine, driver, resolution, build profile, cache state and sampling duration constant. Capture at least five valid windows per run, repeat runs, and review visual output independently.

`python vortex_compare.py baseline.log candidate.log --min-samples 5`

The report shows median frame-window FPS change, frame-time change and pipeline-drop change. A faster result with missing pipeline draws is not an accepted optimization. A passing drop guard is not proof of complete rendering. Missing drop data is reported as unknown, not zero. This tool does not launch or modify any executable.

The Runtime baseline CI checks compilation and synthetic tests of the pinned public upstream on Windows; hosted CI does not establish real-GPU performance.

## Historical local observations

See [development status](DEVELOPMENT_STATUS.md) for the preliminary local test counts, frame-window measurements and limitations as of November 24, 2026. Those observations are not a controlled benchmark; do not use them to claim an improvement from a Vortex patch. Keep the original workload data private unless it is authorized for redistribution.

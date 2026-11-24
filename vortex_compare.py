"""Compare window telemetry from two equivalent, authorized benchmark runs."""
import argparse
import json
from pathlib import Path
from vortex_metrics import summarize

def compare(baseline, candidate, min_samples=5, drop_tolerance=0.0):
    if min_samples < 1 or drop_tolerance < 0:
        raise ValueError("Invalid comparison thresholds")
    a, b = summarize(baseline), summarize(candidate)
    if min(a["sample_count"], b["sample_count"]) < min_samples:
        raise ValueError("Insufficient frame-window samples in one or both runs")
    fps_delta = (b["median_window_fps"] / a["median_window_fps"] - 1) * 100
    time_delta = (b["median_window_ms_per_frame"] / a["median_window_ms_per_frame"] - 1) * 100
    complete = all(x["drop_sample_count"] == x["sample_count"] for x in (a, b))
    drop_delta = (b["median_dropped_draws_per_frame"] - a["median_dropped_draws_per_frame"]) if complete else None
    return {
        "baseline": a, "candidate": b,
        "median_fps_change_pct": round(fps_delta, 4),
        "median_frame_time_change_pct": round(time_delta, 4),
        "median_drop_change": drop_delta,
        "drop_guard": "unknown" if drop_delta is None else ("regression" if drop_delta > drop_tolerance else "pass"),
        "note": "Window medians only; not proof of visual correctness or statistical significance.",
    }

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("baseline", type=Path)
    p.add_argument("candidate", type=Path)
    p.add_argument("--min-samples", type=int, default=5)
    args = p.parse_args()
    try:
        with args.baseline.open(encoding="utf-8", errors="replace") as a, args.candidate.open(encoding="utf-8", errors="replace") as b:
            print(json.dumps(compare(a, b, args.min_samples), indent=2, allow_nan=False))
    except (OSError, ValueError) as exc:
        p.exit(1, f"error: {exc}\n")

if __name__ == "__main__":
    main()

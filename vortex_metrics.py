"""Summarize periodic frame-window telemetry without executing a program."""
import argparse
import json
import math
import re
import statistics
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
FRAME = re.compile(r"(?<![\d.-])(\d+(?:\.\d+)?) fps, (\d+(?:\.\d+)?) ms/frame\b")
DETAIL = re.compile(r"dropped (\d+(?:\.\d+)?) without a host pipeline")

def summarize(lines):
    fps, ms, drops = [], [], []
    for raw in lines:
        line = ANSI.sub("", raw)
        match = FRAME.search(line)
        if not match:
            continue
        f, t = map(float, match.groups())
        if not math.isfinite(f) or not math.isfinite(t) or f <= 0 or t <= 0:
            continue
        fps.append(f)
        ms.append(t)
        drop = DETAIL.search(line)
        if drop:
            value = float(drop.group(1))
            if math.isfinite(value):
                drops.append(value)
    if not fps:
        raise ValueError("No valid frame-window samples")
    return {
        "sample_count": len(fps),
        "median_window_fps": statistics.median(fps),
        "median_window_ms_per_frame": statistics.median(ms),
        "minimum_window_fps": min(fps),
        "maximum_window_fps": max(fps),
        "drop_sample_count": len(drops),
        "median_dropped_draws_per_frame": statistics.median(drops) if drops else None,
    }

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("log", type=Path)
    args = p.parse_args()
    try:
        with args.log.open(encoding="utf-8", errors="replace") as log:
            print(json.dumps(summarize(log), indent=2, allow_nan=False))
    except (OSError, ValueError) as exc:
        p.exit(1, f"error: {exc}\n")

if __name__ == "__main__":
    main()

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, ensure};

const CSV_HEADER: &str = "run_unix_us,output,frame,elapsed_us,interval_us,width,height";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Output {
    WindowQueue,
    OffscreenQueue,
}

impl Output {
    fn label(self) -> &'static str {
        match self {
            Self::WindowQueue => "Vulkan window presents queued",
            Self::OffscreenQueue => "Vulkan offscreen submissions",
        }
    }
}

#[derive(Default)]
struct Run {
    events: usize,
    elapsed_us: u64,
    intervals: Vec<u64>,
}

#[derive(Debug, PartialEq)]
struct Rates {
    average: f64,
    minimum: f64,
    one_percent_low: f64,
    point_one_percent_low: f64,
}

fn measures(intervals: &mut [u64]) -> Option<Rates> {
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_unstable_by(|a, b| b.cmp(a));
    let slow_rate = |n: usize| {
        let sum: f64 = intervals[..n].iter().map(|&us| us as f64).sum();
        (n as f64) * 1_000_000.0 / sum
    };
    let n = intervals.len();
    Some(Rates {
        average: slow_rate(n),
        minimum: slow_rate(1),
        one_percent_low: slow_rate(n.div_ceil(100)),
        point_one_percent_low: slow_rate(n.div_ceil(1000)),
    })
}

fn read_runs(reader: impl BufRead) -> Result<BTreeMap<(i64, Output), Run>> {
    let mut lines = reader.lines();
    ensure!(
        lines.next().transpose()?.as_deref() == Some(CSV_HEADER),
        "invalid Vortex timing header"
    );
    let mut runs: BTreeMap<_, Run> = BTreeMap::new();
    for (i, line) in lines.enumerate() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let row_number = i + 2;
        let mut fields = line.split(',');
        let row: [Option<&str>; 7] = std::array::from_fn(|_| fields.next());
        let [
            Some(id),
            Some(output),
            Some(frame),
            Some(elapsed),
            Some(interval),
            Some(width),
            Some(height),
        ] = row
        else {
            anyhow::bail!("incomplete timing row {row_number}");
        };
        ensure!(
            fields.next().is_none(),
            "extra fields on timing row {row_number}"
        );
        let id = id
            .parse::<i64>()
            .with_context(|| format!("run number on row {row_number}"))?;
        let output = match output {
            "present_queued" => Output::WindowQueue,
            "offscreen_submit" => Output::OffscreenQueue,
            _ => anyhow::bail!("unknown output class on timing row {row_number}"),
        };
        let frame = frame
            .parse::<usize>()
            .with_context(|| format!("frame index on row {row_number}"))?;
        let elapsed = elapsed
            .parse::<u64>()
            .with_context(|| format!("elapsed time on row {row_number}"))?;
        let width = width
            .parse::<u32>()
            .with_context(|| format!("width on row {row_number}"))?;
        let height = height
            .parse::<u32>()
            .with_context(|| format!("height on row {row_number}"))?;
        let run = runs.entry((id, output)).or_default();
        ensure!(
            frame == run.events + 1 && elapsed >= run.elapsed_us && width > 0 && height > 0,
            "invalid frame, time or extent on row {row_number}"
        );
        if frame == 1 {
            ensure!(
                interval.is_empty() && elapsed == 0,
                "invalid first sample on row {row_number}"
            );
        } else {
            let interval = interval
                .parse::<u64>()
                .with_context(|| format!("frame interval on row {row_number}"))?;
            if interval > 0 {
                run.intervals.push(interval);
            }
        }
        run.events = frame;
        run.elapsed_us = elapsed;
    }
    Ok(runs)
}

pub fn summarize(path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let runs = read_runs(BufReader::new(file))?;
    ensure!(!runs.is_empty(), "no Vulkan timing samples found");
    for ((id, output), mut run) in runs {
        let seconds = run.elapsed_us as f64 / 1_000_000.0;
        println!(
            "{} — run {id}, {} events over {seconds:.3} s",
            output.label(),
            run.events
        );
        if let Some(rates) = measures(&mut run.intervals) {
            println!(
                "  average {:.2}/s, minimum {:.2}/s, 1% low {:.2}/s, 0.1% low {:.2}/s",
                rates.average, rates.minimum, rates.one_percent_low, rates.point_one_percent_low,
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_rates_use_the_mean_interval_of_the_slowest_frames() {
        let mut times = vec![10_000; 200];
        times[0] = 20_000;
        times[1] = 30_000;
        let rates = measures(&mut times).unwrap();
        assert_eq!(rates.one_percent_low, 40.0);
        assert_eq!(rates.point_one_percent_low, 1_000_000.0 / 30_000.0);
        assert_eq!(rates.minimum, rates.point_one_percent_low);
    }

    #[test]
    fn window_and_offscreen_events_have_separate_runs() {
        let csv = format!(
            "{CSV_HEADER}\n1,present_queued,1,0,,720,480\n1,offscreen_submit,1,0,,720,480\n"
        );
        let runs = read_runs(csv.as_bytes()).unwrap();
        assert_eq!(runs.len(), 2);
        assert!(
            runs.values()
                .all(|run| run.events == 1 && run.intervals.is_empty())
        );
    }
}

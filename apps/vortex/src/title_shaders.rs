use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

pub(crate) fn compile(game: &Path, work: &Path, jobs: usize, emulate: bool) -> Result<()> {
    vortex_core::ensure_outside_repo(game)?;
    vortex_core::ensure_outside_repo(work)?;
    ensure!((1..=256).contains(&jobs), "jobs must be between 1 and 256");
    let compiler = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/vortex-shader/native/prepare.py");
    ensure!(compiler.is_file(), "shader compiler source missing at {}", compiler.display());
    let python = std::env::var_os("VORTEX_PYTHON").unwrap_or_else(|| "python".into());
    let mut command = Command::new(python);
    command.args(["-I", "-B"]).arg(&compiler)
        .arg("--game-dir").arg(game)
        .arg("--work").arg(work)
        .arg("--jobs").arg(jobs.to_string());
    if emulate {
        command.arg("--wave64-emulate");
    }
    let status = command.status().context("starting the Vortex shader compiler")?;
    ensure!(status.success(), "Vortex shader compiler exited with {status}");
    Ok(())
}

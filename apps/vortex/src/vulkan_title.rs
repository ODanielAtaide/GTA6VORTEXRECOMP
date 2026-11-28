//! Launch the separately built Leonida runtime with its native Vulkan device.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::Console;

pub(crate) fn run(exe: &Path, runtime: &Path, console: Console, args: &[String]) -> Result<()> {
    let exe = std::path::absolute(exe)?;
    let runtime = std::path::absolute(runtime)?;
    let launcher = runtime.join("gta6rt.exe");
    let overlay = runtime.join("overlay");
    for path in [&exe, &runtime, &overlay] {
        vortex_core::ensure_outside_repo(path)?;
    }
    ensure!(
        exe.is_file(),
        "title executable not found at {}",
        exe.display()
    );
    ensure!(
        launcher.is_file(),
        "runtime executable not found at {}",
        launcher.display()
    );
    ensure!(
        runtime.join("gta6rt_tls_host.dll").is_file(),
        "runtime TLS helper missing from {}",
        runtime.display()
    );
    let capability = Command::new(&launcher)
        .arg("--vortex-vulkan-runtime")
        .current_dir(&runtime)
        .output()
        .with_context(|| format!("checking Vulkan support in {}", launcher.display()))?;
    ensure!(
        capability.status.success()
            && String::from_utf8_lossy(&capability.stdout).trim() == "vortex-vulkan-runtime=1",
        "the runtime at {} was not built with Vortex's Vulkan-only integration",
        launcher.display()
    );
    let shader_path = runtime.join("shader_path.txt");
    let shader_path = fs::read_to_string(&shader_path).with_context(|| {
        format!(
            "reading prepared shader path from {}",
            shader_path.display()
        )
    })?;
    let shader_path = Path::new(shader_path.trim_end_matches(['\r', '\n']));
    ensure!(
        shader_path.is_absolute(),
        "the prepared shader path must be absolute"
    );
    vortex_core::ensure_outside_repo(shader_path)?;
    ensure!(
        shader_path.join("index.json").is_file(),
        "shader index missing from {}",
        shader_path.display()
    );
    let game_dir = exe
        .parent()
        .context("title executable has no package directory")?;
    let device_type = match console {
        Console::SeriesX => "7",
        Console::SeriesS => "6",
    };
    let mut command = Command::new(&launcher);
    command
        .current_dir(&runtime)
        .env("LEONIDA_D3D12", "vulkan")
        .env("LEONIDA_DEVICE_TYPE", device_type)
        .env("LEONIDA_SHADERS", shader_path)
        .env("LEONIDA_UNIMPLEMENTED", "stop")
        .env_remove("LEONIDA_D3D12_LISTS")
        .env_remove("LEONIDA_COMPAT_DIR")
        .env_remove("LEONIDA_WRITE_DIR")
        .env_remove("LEONIDA_STUB_PIPELINES")
        .env_remove("LEONIDA_SKIP_COMPUTE")
        .arg("--run")
        .arg(&exe)
        .arg("--game-dir")
        .arg(game_dir)
        .arg("--overlay")
        .arg(&overlay);
    if !args.is_empty() {
        command.arg("--").args(args);
    }
    let status = command
        .status()
        .with_context(|| format!("starting {}", launcher.display()))?;
    ensure!(
        status.success(),
        "Vulkan title runtime exited with {status}"
    );
    Ok(())
}

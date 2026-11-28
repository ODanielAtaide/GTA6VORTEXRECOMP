//! `vortex`: the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).

mod census;
mod inspect;
mod run;
mod shader;
mod translate;
#[cfg(feature = "vulkan")]
mod precompile;
#[cfg(all(windows, feature = "vulkan"))]
mod vulkan_title;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "vortex",
    version,
    about = "Run an Xbox Series title natively on Windows"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found.
        #[arg(long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`VORTEX_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
    },
    /// Read-only inventory and offline Vulkan shader compatibility survey.
    #[cfg(feature = "vulkan")]
    VulkanPrecompile {
        #[arg(long)]
        game_dir: PathBuf,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        /// Validate diagnostic SPIR-V using spirv-val from PATH.
        #[arg(long)]
        validate: bool,
        /// Reanalyze containers instead of using the metadata cache.
        #[arg(long)]
        refresh: bool,
        /// Restrict the scan to EXE, DLL and SGD5 files; skip opaque archives.
        #[arg(long)]
        code_only: bool,
    },
    /// Verify the shared Vulkan host library from the main executable.
    #[cfg(feature = "vulkan")]
    VulkanCheck {
        /// Optional compiled, validated bindless.comp SPIR-V fixture.
        #[arg(long)]
        bindless_shader: Option<PathBuf>,
        /// Also run the bounded asynchronous submission regression.
        #[arg(long)]
        async_check: bool,
    },
    /// Verify the reusable Vulkan buffer command executor.
    #[cfg(feature = "vulkan")]
    VulkanCommands,
    /// Verify reusable depth-tested drawing with the original shader fixture.
    #[cfg(feature = "vulkan")]
    VulkanDraw { vertex: PathBuf, fragment: PathBuf },
    /// Verify the reusable compute dispatch executor.
    #[cfg(feature = "vulkan")]
    VulkanDispatch { shader: PathBuf },
    /// Verify direct GFX10 diagnostic shader compilation and Vulkan dispatch.
    #[cfg(feature = "vulkan")]
    VulkanNative,
    /// Emit or run native vertex and fragment stage regression.
    #[cfg(feature = "vulkan")]
    VulkanNativeGraphics {
        #[arg(long)]
        emit_only: Option<PathBuf>,
    },
    /// Emit or execute advanced native CFG compute regressions.
    #[cfg(feature = "vulkan")]
    VulkanAdvanced {
        #[arg(long)]
        emit_only: Option<PathBuf>,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        exe: PathBuf,
        /// Graphics backend used to run the title.
        #[arg(long, value_enum, default_value_t = Backend::D3d12)]
        backend: Backend,
        /// Use a Leonida runtime built with Vulkan and prepared for this title.
        #[arg(long, value_name = "DIRECTORY", requires = "execute")]
        vulkan_runtime: Option<PathBuf>,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Show the resolved configuration and check it.
    Config,
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    D3d12,
    Vulkan,
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

fn main() -> Result<()> {
    vortex_core::logging::init("info");
    // The audio recorder `run` starts for `VORTEX_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if vortex_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect {
            file,
            imports,
            exports,
        } => inspect::run(&file, imports, exports),
        Command::Census {
            paths,
            strict,
            missing,
        } => census::run(paths, strict, missing),
        Command::LibExports { lib, list } => inspect::lib_exports(&lib, list),
        Command::ShaderInfo {
            paths,
            verbose,
            disasm,
        } => shader::run(paths, verbose || disasm, disasm),
        Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            jobs,
        } => {
            // `VORTEX_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped
            if let Some(n) = std::env::var("VORTEX_VIEW_LIMIT").ok().and_then(|v| v.parse().ok()) {
                vortex_shader::emit::set_view_heap_limit(n);
            }
            // `VORTEX_HOST_WAVE=32`: translate as for a GPU whose waves are at most 32 lanes
            if let Some(n) = std::env::var("VORTEX_HOST_WAVE").ok().and_then(|v| v.parse().ok()) {
                vortex_shader::emit::set_host_max_wave(n);
            }
            translate::run(paths, validate, pipelines, list, jobs)
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanPrecompile { game_dir, workers, validate, refresh, code_only } =>
            precompile::run(&game_dir, workers, validate, refresh, code_only),
        #[cfg(feature = "vulkan")]
        Command::VulkanCheck { bindless_shader, async_check } => {
            let mut host = vortex_vulkan::Context::new()
                .map_err(|e| anyhow::anyhow!("Vulkan initialization: {e}"))?;
            host.verify_transfers().map_err(|e| anyhow::anyhow!("Vulkan transfers: {e}"))?;
            if let Some(path) = bindless_shader {
                host.verify_bindless(&path).map_err(|e| anyhow::anyhow!("Vulkan descriptors: {e}"))?;
            }
            if async_check {
                host.verify_async().map_err(|e| anyhow::anyhow!("Vulkan submission: {e}"))?;
            }
            println!("PASS: shared Vulkan host library verified from vortex CLI (not title execution)");
            Ok(())
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanAdvanced { emit_only } => {
            if let Some(dir) = emit_only {
                vortex_vulkan::Context::emit_advanced(&dir)
                    .map_err(|e| anyhow::anyhow!("Advanced native emission: {e}"))
            } else {
                let mut host = vortex_vulkan::Context::new()
                    .map_err(|e| anyhow::anyhow!("Vulkan initialization: {e}"))?;
                host.verify_advanced().map_err(|e| anyhow::anyhow!("Advanced native shader executor: {e}"))
            }
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanNativeGraphics { emit_only } => {
            if let Some(dir)=emit_only {
                vortex_vulkan::Context::emit_native_graphics(&dir)
                    .map_err(|e|anyhow::anyhow!("Native graphics emission: {e}"))
            } else {
                let mut host=vortex_vulkan::Context::new()
                    .map_err(|e|anyhow::anyhow!("Vulkan initialization: {e}"))?;
                host.verify_native_graphics().map_err(|e|anyhow::anyhow!("Native graphics executor: {e}"))
            }
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanNative => {
            let mut host = vortex_vulkan::Context::new()
                .map_err(|e| anyhow::anyhow!("Vulkan initialization: {e}"))?;
            host.verify_native_dispatch().map_err(|e| anyhow::anyhow!("Native Vulkan shader executor: {e}"))
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanDispatch { shader } => {
            let mut host = vortex_vulkan::Context::new()
                .map_err(|e| anyhow::anyhow!("Vulkan initialization: {e}"))?;
            host.verify_dispatch(&shader).map_err(|e| anyhow::anyhow!("Vulkan dispatch executor: {e}"))
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanDraw { vertex, fragment } => {
            let mut host = vortex_vulkan::Context::new()
                .map_err(|e| anyhow::anyhow!("Vulkan initialization: {e}"))?;
            host.verify_draws(&vertex,&fragment).map_err(|e| anyhow::anyhow!("Vulkan draw executor: {e}"))
        }
        #[cfg(feature = "vulkan")]
        Command::VulkanCommands => {
            let mut host = vortex_vulkan::Context::new()
                .map_err(|e| anyhow::anyhow!("Vulkan initialization: {e}"))?;
            host.verify_commands().map_err(|e| anyhow::anyhow!("Vulkan command executor: {e}"))
        }
        Command::Run {
            exe,
            backend,
            vulkan_runtime,
            unimplemented,
            strict,
            execute,
            console,
            title_args,
        } => {
            if let Some(runtime) = vulkan_runtime {
                anyhow::ensure!(
                    matches!(backend, Backend::Vulkan),
                    "--vulkan-runtime requires --backend vulkan"
                );
                anyhow::ensure!(
                    !strict,
                    "--strict applies to Vortex's in-process import resolver"
                );
                anyhow::ensure!(
                    matches!(unimplemented, Unimplemented::Trap),
                    "the Vulkan runtime stops on unimplemented imports"
                );
                #[cfg(all(windows, feature = "vulkan"))]
                return vulkan_title::run(&exe, &runtime, console, &title_args);
                #[cfg(not(all(windows, feature = "vulkan")))]
                {
                    let _ = runtime;
                    anyhow::bail!("--vulkan-runtime requires Windows and --features vulkan");
                }
            }
            let _ = title_args;
            if matches!(backend, Backend::Vulkan) {
                #[cfg(all(windows, feature = "vulkan"))]
                {
                    vortex_hle::modules::d3d12x::enable_native_vulkan();
                    println!("LIVE_VULKAN_REQUESTED: guest runtime, no host D3D12 fallback");
                }
                #[cfg(not(all(windows, feature = "vulkan")))]
                anyhow::bail!("Live Vulkan requires Windows and --features vulkan");
            }
            run::run(&exe, unimplemented, strict, execute, console)
        }
        Command::Config => show_config(),
    }
}

fn show_config() -> Result<()> {
    let cfg = vortex_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = vortex_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = vortex_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see vortex.example.toml)"
    );
    Ok(all)
}

//! `vortex`: the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).

mod benchmark;
mod census;
mod inspect;
mod run;
mod shader;
#[cfg(feature = "vulkan")]
mod precompile;
#[cfg(feature = "vulkan")]
mod title_shaders;

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
    /// Compile the game's RDNA2 shader pack for the native Vulkan renderer.
    #[cfg(feature = "vulkan")]
    CompileTitleShaders {
        #[arg(long)]
        game_dir: PathBuf,
        #[arg(long)]
        work: PathBuf,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        #[arg(long)]
        wave64_emulate: bool,
    },
    /// Precompile compute pipelines into the native Vulkan cache without launching the title.
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
    PrecompileVulkanPipelines {
        #[arg(long)]
        shader_pack: Option<PathBuf>,
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=16))]
        jobs: u32,
        /// Limit the number of pipelines built. Zero builds every selected pipeline.
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Only build entries whose names contain this text.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Summarize a Vortex frame timing CSV.
    Benchmark { samples: PathBuf },
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
        #[arg(long, value_enum, default_value_t = Backend::Vulkan)]
        backend: Backend,
        /// Local SPIR-V pack created by compile-title-shaders.
        #[arg(long, value_name = "DIRECTORY", requires = "execute")]
        shader_pack: Option<PathBuf>,
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
        #[cfg(feature = "vulkan")]
        Command::VulkanPrecompile { game_dir, workers, validate, refresh, code_only } =>
            precompile::run(&game_dir, workers, validate, refresh, code_only),
        Command::Benchmark { samples } => benchmark::summarize(&samples),
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
            backend: Backend::Vulkan,
            shader_pack,
            unimplemented,
            strict,
            execute,
            console,
            title_args,
        } => {
            let _ = title_args;
            if execute {
                anyhow::ensure!(
                    matches!(unimplemented, Unimplemented::Trap),
                    "execution requires --unimplemented trap"
                );
                #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
                {
                    vortex_hle::modules::d3d12x::enable_native_vulkan();
                    println!("LIVE_VULKAN_REQUESTED: in-process Vortex runtime");
                }
                #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan")))]
                anyhow::bail!("Live Vulkan requires Windows x64 MSVC and --features vulkan");
            }
            run::run(&exe, unimplemented, strict, execute, console, shader_pack.as_deref())
        }
        #[cfg(feature = "vulkan")]
        Command::CompileTitleShaders { game_dir, work, jobs, wave64_emulate } =>
            title_shaders::compile(&game_dir, &work, jobs, wave64_emulate),
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
        Command::PrecompileVulkanPipelines { shader_pack, jobs, limit, filter } =>
            run::precompile_vulkan(shader_pack.as_deref(), jobs, limit, filter.as_deref()),
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

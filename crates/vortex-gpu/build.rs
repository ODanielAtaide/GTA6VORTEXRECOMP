use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if env::var_os("CARGO_FEATURE_VULKAN").is_none()
        || env::var("TARGET").as_deref() != Ok("x86_64-pc-windows-msvc")
    {
        return;
    }
    println!("cargo:rerun-if-changed=native");
    for name in ["CMAKE", "VORTEX_PYTHON", "VULKAN_SDK"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native-graphics");
    let cmake = cmake();
    let mut configure = Command::new(&cmake);
    configure
        .args(["-S", "native", "-B"])
        .arg(&out)
        .args(["-G", "Visual Studio 17 2022", "-A", "x64"]);
    if let Some(python) = env::var_os("VORTEX_PYTHON") {
        let mut option = std::ffi::OsString::from("-DPython3_EXECUTABLE=");
        option.push(python);
        configure.arg(option);
    }
    run(&mut configure);
    run(Command::new(cmake)
        .arg("--build")
        .arg(&out)
        .args(["--config", "Release", "--target", "vortex_native_graphics"])
        .args(["--", "/nr:false"]));
    println!("cargo:rustc-link-search=native={}", out.join("Release").display());
    println!("cargo:rustc-link-lib=static=vortex_native_graphics");
    for library in ["ole32", "windowscodecs", "xmllite", "shlwapi", "user32", "gdi32", "psapi"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

fn run(command: &mut Command) {
    let status = command.status().unwrap_or_else(|error| panic!("{command:?}: {error}"));
    assert!(status.success(), "{command:?} exited with {status}");
}

fn cmake() -> PathBuf {
    if let Some(cmake) = env::var_os("CMAKE") {
        return cmake.into();
    }
    if let Some(installation) = env::var_os("VSINSTALLDIR").map(PathBuf::from)
        && let Some(path) = bundled_cmake(&installation)
    {
        return path;
    }
    if let Some(program_files) = env::var_os("ProgramFiles(x86)") {
        let vswhere = PathBuf::from(program_files).join("Microsoft Visual Studio/Installer/vswhere.exe");
        if let Ok(output) = Command::new(vswhere)
            .args(["-latest", "-products", "*", "-version", "[17.0,18.0)", "-property", "installationPath"])
            .output()
            && output.status.success()
            && let Ok(installation) = std::str::from_utf8(&output.stdout)
            && let Some(path) = bundled_cmake(Path::new(installation.trim()))
        {
            return path;
        }
    }
    PathBuf::from("cmake")
}

fn bundled_cmake(installation: &Path) -> Option<PathBuf> {
    let path = installation.join("Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe");
    path.is_file().then_some(path)
}

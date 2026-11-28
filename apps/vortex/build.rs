fn main() {
    if std::env::var("TARGET").as_deref() == Ok("x86_64-pc-windows-msvc")
        && std::env::var_os("CARGO_FEATURE_VULKAN").is_some()
    {
        // Old source-level regression tooling remains available without
        // pulling any Direct3D module into a Vulkan title process at startup.
        for dll in ["d3d12.dll", "dxgi.dll", "d3dcompiler_47.dll"] {
            println!("cargo:rustc-link-arg-bin=vortex=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-lib=delayimp");
    }
}

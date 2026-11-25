// SPDX-License-Identifier: GPL-3.0-or-later
use amd_vulkan_probe::{Context, Result};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [flag, output] = args.as_slice() {
        if flag == "--emit-loop" {
            return amd_vulkan_probe::spirv::write_loop(Path::new(output));
        }
        if flag == "--emit-branch" {
            return amd_vulkan_probe::spirv::write_branch(Path::new(output));
        }
        if flag == "--emit-float" {
            return amd_vulkan_probe::spirv::write_float(Path::new(output));
        }
        if flag == "--emit-conditional" {
            return amd_vulkan_probe::spirv::write_conditional(Path::new(output));
        }
        if flag == "--emit-mixed" {
            return amd_vulkan_probe::spirv::write_mixed(Path::new(output));
        }
        if flag == "--emit-demo" {
            return amd_vulkan_probe::spirv::write_demo(Path::new(output));
        }
    }
    let mut context = Context::new()?;
    match args.as_slice() {
        [flag, size, path] if flag == "--verify-wave" => {
            let size: u32 = size.to_str().ok_or("Invalid wave size")?.parse()?;
            context.compute_wave(Path::new(path), size)
        }
        [flag, vert, frag] if flag == "--graphics" => context.graphics(Path::new(vert), Path::new(frag)),
        [flag, compute, vert, frag] if flag == "--combined" => {
            context.compute(Path::new(compute))?;
            context.graphics(Path::new(vert), Path::new(frag))?;
            println!("PASS: compute and graphics on the same Vulkan device");
            Ok(())
        }
        [flag, path] if flag == "--verify-loop" => context.compute_loop(Path::new(path)),
        [flag, path] if flag == "--verify-branch" => context.compute_branch(Path::new(path)),
        [flag, path] if flag == "--verify-float" => context.compute_float(Path::new(path)),
        [flag, path] if flag == "--verify-conditional" => context.compute_conditional(Path::new(path)),
        [flag, path] if flag == "--verify-mixed" => context.compute_mixed(Path::new(path)),
        [path] => context.compute(Path::new(path)),
        [] => Ok(()),
        _ => Err("Usage: amd-vulkan-probe [--emit-loop out.spv | --verify-loop in.spv | --emit-branch out.spv | --verify-branch in.spv | --verify-wave 32|64 wave.spv | --emit-float out.spv | --verify-float in.spv | --emit-conditional out.spv | --verify-conditional in.spv | --emit-demo out.spv | --emit-mixed out.spv | --verify-mixed mixed.spv | compute.spv | --graphics vertex.spv fragment.spv | --combined compute.spv vertex.spv fragment.spv]".into()),
    }
}

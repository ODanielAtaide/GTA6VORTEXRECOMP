// SPDX-License-Identifier: GPL-3.0-or-later
use amd_vulkan_probe::{Context, Result};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [flag, output] = args.as_slice() {
        if flag == "--emit-cmpx32" || flag == "--emit-cmpx64" {
            let wave64 = flag == "--emit-cmpx64";
            let words = vortex_shader::spirv_vector::cmpx_demo(wave64)?;
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            std::fs::write(output, bytes)?;
            println!("PASS: decoded CMPX updates for wave{}, {} SPIR-V words", if wave64 {64} else {32}, words.len());
            return Ok(());
        }
        if flag == "--emit-exec32" || flag == "--emit-exec64" {
            let wave64 = flag == "--emit-exec64";
            let words = vortex_shader::spirv_vector::exec_demo(wave64)?;
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            std::fs::write(output, bytes)?;
            println!("PASS: decoded EXEC updates for wave{}, {} SPIR-V words", if wave64 {64} else {32}, words.len());
            return Ok(());
        }
        if flag == "--emit-vector32" || flag == "--emit-vector64" {
            let wave64 = flag == "--emit-vector64";
            let words = vortex_shader::spirv_vector::demo(wave64)?;
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            std::fs::write(output, bytes)?;
            println!("PASS: decoded vector GFX10 wave{} fixture, {} SPIR-V words", if wave64 {64} else {32}, words.len());
            return Ok(());
        }
        if flag == "--emit-carry-demo" {
            let words = vortex_shader::spirv_scalar::carry_demo()?;
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            std::fs::write(output, bytes)?;
            println!("PASS: decoded GFX10 carry/borrow fixture and emitted {} SPIR-V words", words.len());
            return Ok(());
        }
        if flag == "--emit-scalar-demo" {
            let words = vortex_shader::spirv_scalar::demo()?;
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            std::fs::write(output, bytes)?;
            println!("PASS: decoded GFX10 scalar fixture and emitted {} SPIR-V words", words.len());
            return Ok(());
        }
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
        [flag, vert, frag] if flag == "--indexed" => context.graphics_indexed(Path::new(vert), Path::new(frag)),
        [flag, vert, frag] if flag == "--texture" => context.graphics_texture(Path::new(vert), Path::new(frag)),
        [flag, vert, frag] if flag == "--graphics" => context.graphics(Path::new(vert), Path::new(frag)),
        [flag, compute, vert, frag] if flag == "--combined" => {
            context.compute(Path::new(compute))?;
            context.graphics(Path::new(vert), Path::new(frag))?;
            println!("PASS: compute and graphics on the same Vulkan device");
            Ok(())
        }
        [flag, path] if flag == "--verify-cmpx32" => context.compute_cmpx_demo(Path::new(path), false),
        [flag, path] if flag == "--verify-cmpx64" => context.compute_cmpx_demo(Path::new(path), true),
        [flag, path] if flag == "--verify-exec32" => context.compute_exec_demo(Path::new(path), false),
        [flag, path] if flag == "--verify-exec64" => context.compute_exec_demo(Path::new(path), true),
        [flag, path] if flag == "--verify-vector32" => context.compute_vector_demo(Path::new(path), false),
        [flag, path] if flag == "--verify-vector64" => context.compute_vector_demo(Path::new(path), true),
        [flag, path] if flag == "--verify-carry-demo" => context.compute_carry_demo(Path::new(path)),
        [flag, path] if flag == "--verify-scalar-demo" => context.compute_scalar_demo(Path::new(path)),
        [flag, path] if flag == "--verify-loop" => context.compute_loop(Path::new(path)),
        [flag, path] if flag == "--verify-branch" => context.compute_branch(Path::new(path)),
        [flag, path] if flag == "--verify-float" => context.compute_float(Path::new(path)),
        [flag, path] if flag == "--verify-conditional" => context.compute_conditional(Path::new(path)),
        [flag, path] if flag == "--verify-mixed" => context.compute_mixed(Path::new(path)),
        [path] => context.compute(Path::new(path)),
        [] => Ok(()),
        _ => Err("Usage: amd-vulkan-probe [--emit-cmpx32 out.spv | --emit-cmpx64 out.spv | --verify-cmpx32 in.spv | --verify-cmpx64 in.spv | --emit-exec32 out.spv | --emit-exec64 out.spv | --verify-exec32 in.spv | --verify-exec64 in.spv | --emit-vector32 out.spv | --emit-vector64 out.spv | --verify-vector32 in.spv | --verify-vector64 in.spv | --emit-carry-demo out.spv | --verify-carry-demo in.spv | --emit-scalar-demo out.spv | --verify-scalar-demo in.spv | --indexed vertex.spv fragment.spv | --texture vertex.spv texture.spv | --emit-loop out.spv | --verify-loop in.spv | --emit-branch out.spv | --verify-branch in.spv | --verify-wave 32|64 wave.spv | --emit-float out.spv | --verify-float in.spv | --emit-conditional out.spv | --verify-conditional in.spv | --emit-demo out.spv | --emit-mixed out.spv | --verify-mixed mixed.spv | compute.spv | --graphics vertex.spv fragment.spv | --combined compute.spv vertex.spv fragment.spv]".into()),
    }
}

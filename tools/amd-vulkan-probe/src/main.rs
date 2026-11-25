// SPDX-License-Identifier: GPL-3.0-or-later
use amd_vulkan_probe::{Context, Result};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [flag, output] = args.as_slice() {
        if flag == "--emit-demo" {
            return amd_vulkan_probe::spirv::write_demo(Path::new(output));
        }
    }
    let mut context = Context::new()?;
    match args.as_slice() {
        [flag, vert, frag] if flag == "--graphics" => context.graphics(Path::new(vert), Path::new(frag)),
        [flag, compute, vert, frag] if flag == "--combined" => {
            context.compute(Path::new(compute))?;
            context.graphics(Path::new(vert), Path::new(frag))?;
            println!("PASS: compute and graphics on the same Vulkan device");
            Ok(())
        }
        [path] => context.compute(Path::new(path)),
        [] => Ok(()),
        _ => Err("Usage: amd-vulkan-probe [compute.spv | --graphics vertex.spv fragment.spv | --combined compute.spv vertex.spv fragment.spv]".into()),
    }
}

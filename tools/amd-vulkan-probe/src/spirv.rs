// SPDX-License-Identifier: GPL-3.0-or-later
//! A deliberately small, independent compute IR. No console input is parsed.
//! Emits a bounded synthetic kernel using a public SPIR-V instruction encoding.
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Index,
    Constant(u32),
    Add(usize, usize),
    Mul(usize, usize),
}

fn instruction(out: &mut Vec<u32>, opcode: u16, args: &[u32]) {
    out.push((((args.len() + 1) as u32) << 16) | opcode as u32);
    out.extend_from_slice(args);
}

/// Emit a kernel that writes one u32 per invocation to descriptor 0:0.
/// Dispatch must match the size of the output buffer, local size 64.
/// References must name earlier SSA values.
pub fn emit(ops: &[Op], result: usize) -> crate::Result<Vec<u32>> {
    if ops.is_empty() || ops.len() > 4096 || result >= ops.len() {
        return Err("Invalid IR size or result".into());
    }
    for (i, op) in ops.iter().enumerate() {
        if let Op::Add(a, b) | Op::Mul(a, b) = *op {
            if a >= i || b >= i { return Err("Invalid forward SSA reference".into()); }
        }
    }
    // Fixed IDs 1..16: types, globals, zero, function, label and index.
    // Each IR node receives one fresh result ID starting at 17.
    let id = |i: usize| 17 + i as u32;
    let ptr = id(ops.len());
    let mut out = vec![0x07230203, 0x00010300, 0, ptr + 1, 0];
    let op = |out: &mut Vec<u32>, code, args: &[u32]| instruction(out, code, args);
    op(&mut out, 17, &[1]); // Shader capability
    op(&mut out, 14, &[0, 1]); // Logical, GLSL450
    // GLCompute, function 12, "main" (null-terminated), Input variable 10.
    op(&mut out, 15, &[5, 12, 0x6e69616d, 0, 10]);
    op(&mut out, 16, &[12, 17, 64, 1, 1]); // LocalSize
    op(&mut out, 71, &[10, 11, 28]); // BuiltIn GlobalInvocationId
    op(&mut out, 71, &[6, 6, 4]); // ArrayStride
    op(&mut out, 71, &[7, 2]); // Block
    op(&mut out, 72, &[7, 0, 35, 0]); // member Offset
    op(&mut out, 71, &[11, 34, 0]); // DescriptorSet
    op(&mut out, 71, &[11, 33, 0]); // Binding
    op(&mut out, 19, &[1]); // void
    op(&mut out, 33, &[2, 1]); // function type
    op(&mut out, 21, &[3, 32, 0]); // uint
    op(&mut out, 23, &[4, 3, 3]); // uvec3
    op(&mut out, 32, &[5, 1, 4]); // Input pointer
    op(&mut out, 29, &[6, 3]); // runtime array
    op(&mut out, 30, &[7, 6]); // block struct
    op(&mut out, 32, &[8, 12, 7]); // StorageBuffer block pointer
    op(&mut out, 32, &[9, 12, 3]); // StorageBuffer uint pointer
    op(&mut out, 59, &[5, 10, 1]); // Input global
    op(&mut out, 59, &[8, 11, 12]); // StorageBuffer global
    op(&mut out, 43, &[3, 13, 0]); // member index zero
    for (i, node) in ops.iter().enumerate() {
        if let Op::Constant(v) = *node { op(&mut out, 43, &[3, id(i), v]); }
    }
    op(&mut out, 54, &[1, 12, 0, 2]); // Function
    op(&mut out, 248, &[14]); // Label
    op(&mut out, 61, &[4, 15, 10]); // Load invocation ID
    op(&mut out, 81, &[3, 16, 15, 0]); // Extract x
    for (i, node) in ops.iter().enumerate() {
        match *node {
            Op::Index => op(&mut out, 83, &[3, id(i), 16]), // CopyObject
            Op::Constant(_) => {}
            Op::Add(a,b) => op(&mut out, 128, &[3, id(i), id(a), id(b)]),
            Op::Mul(a,b) => op(&mut out, 132, &[3, id(i), id(a), id(b)]),
        }
    }
    op(&mut out, 65, &[9, ptr, 11, 13, 16]); // AccessChain
    op(&mut out, 62, &[ptr, id(result)]); // Store
    op(&mut out, 253, &[]); // Return
    op(&mut out, 56, &[]); // FunctionEnd
    Ok(out)
}

pub fn write_demo(path: &Path) -> crate::Result<()> {
    let ops = [Op::Index, Op::Constant(1664525), Op::Mul(0,1),
        Op::Constant(1013904223), Op::Add(2,3)];
    let words = emit(&ops, 4)?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(path, &bytes)?;
    println!("PASS: emitted {} SPIR-V words from {} SSA nodes", words.len(), ops.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_ssa() {
        assert!(emit(&[], 0).is_err());
        assert!(emit(&[Op::Add(0,0)], 0).is_err());
        assert!(emit(&[Op::Index], 1).is_err());
    }
    #[test]
    fn encodes_complete_instruction_stream() {
        let module = emit(&[Op::Index, Op::Constant(7), Op::Mul(0,1), Op::Add(2,1)], 3).unwrap();
        assert_eq!(module[0], 0x07230203);
        let mut at = 5;
        while at < module.len() {
            let n = (module[at] >> 16) as usize;
            assert!(n > 0 && at + n <= module.len());
            at += n;
        }
        assert_eq!(at, module.len());
    }
}

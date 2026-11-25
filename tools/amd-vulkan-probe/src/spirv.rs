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
    Sub(usize, usize),
    And(usize, usize),
    Or(usize, usize),
    Xor(usize, usize),
    Shl(usize, usize),
    Shr(usize, usize),
    SelectUlt(usize, usize, usize, usize),
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
        if let Op::Add(a, b) | Op::Mul(a, b) | Op::Sub(a, b) | Op::And(a, b) | Op::Or(a, b) | Op::Xor(a, b) | Op::Shl(a, b) | Op::Shr(a, b) = *op {
            if a >= i || b >= i { return Err("Invalid forward SSA reference".into()); }
        }
        if let Op::SelectUlt(a,b,t,f) = *op {
            if [a,b,t,f].iter().any(|&v| v >= i) { return Err("Invalid select SSA reference".into()); }
        }
    }
    // IDs 17 and 18 are bool type and shift mask; each node reserves
    // a result and a temporary ID for lowering compound operations.
    let id = |i: usize| 19 + 2 * i as u32;
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
    op(&mut out, 20, &[17]); // bool
    op(&mut out, 23, &[4, 3, 3]); // uvec3
    op(&mut out, 32, &[5, 1, 4]); // Input pointer
    op(&mut out, 29, &[6, 3]); // runtime array
    op(&mut out, 30, &[7, 6]); // block struct
    op(&mut out, 32, &[8, 12, 7]); // StorageBuffer block pointer
    op(&mut out, 32, &[9, 12, 3]); // StorageBuffer uint pointer
    op(&mut out, 59, &[5, 10, 1]); // Input global
    op(&mut out, 59, &[8, 11, 12]); // StorageBuffer global
    op(&mut out, 43, &[3, 13, 0]); // member index zero
    op(&mut out, 43, &[3, 18, 31]); // defined modulo-32 shift count
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
            Op::Sub(a,b) => op(&mut out, 130, &[3, id(i), id(a), id(b)]),
            Op::And(a,b) => op(&mut out, 199, &[3, id(i), id(a), id(b)]),
            Op::Or(a,b) => op(&mut out, 197, &[3, id(i), id(a), id(b)]),
            Op::Xor(a,b) => op(&mut out, 198, &[3, id(i), id(a), id(b)]),
            Op::Shl(a,b) | Op::Shr(a,b) => {
                let tmp = id(i) + 1;
                op(&mut out, 199, &[3, tmp, id(b), 18]);
                let opcode = if matches!(node, Op::Shl(..)) { 196 } else { 194 };
                op(&mut out, opcode, &[3, id(i), id(a), tmp]);
            }
            Op::SelectUlt(a,b,t,f) => {
                let tmp = id(i) + 1;
                op(&mut out, 176, &[17, tmp, id(a), id(b)]);
                op(&mut out, 169, &[3, id(i), tmp, id(t), id(f)]);
            }
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

// Original workload exercising every supported operation and wrapping arithmetic.
pub const MIXED: [Op; 12] = [
    Op::Index, Op::Constant(0xdeadbeef), Op::Xor(0, 1),
    Op::Constant(1664525), Op::Mul(2, 3),
    Op::Constant(0x00ffffff), Op::And(4, 5),
    Op::Constant(0xa5000000), Op::Or(6, 7),
    Op::Sub(8, 0), Op::Constant(1013904223), Op::Add(9, 10),
];

/// A CPU oracle for the fixed test program. Keep it independent of the emitter.
pub fn mixed_expected(index: u32) -> u32 {
    let v = (index ^ 0xdeadbeef).wrapping_mul(1664525);
    ((v & 0x00ffffff) | 0xa5000000)
        .wrapping_sub(index).wrapping_add(1013904223)
}

pub fn write_mixed(path: &Path) -> crate::Result<()> {
    let words = emit(&MIXED, MIXED.len() - 1)?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(path, &bytes)?;
    println!("PASS: emitted {} SPIR-V words from {} mixed SSA nodes", words.len(), MIXED.len());
    Ok(())
}

// Two branches, large shift counts, and high-bit unsigned inputs.
pub const CONDITIONAL: [Op; 10] = [
    Op::Index, Op::Constant(0xf1234567), Op::Xor(0,1),
    Op::Shl(2,0), Op::Shr(2,0), Op::Constant(1), Op::And(0,5),
    Op::SelectUlt(6,5,3,4), Op::Constant(0x12345678), Op::Add(7,8),
];

pub fn conditional_expected(index: u32) -> u32 {
    let v = index ^ 0xf1234567;
    let shifted = if index & 1 == 0 { v.wrapping_shl(index) } else { v.wrapping_shr(index) };
    shifted.wrapping_add(0x12345678)
}

pub fn write_conditional(path: &Path) -> crate::Result<()> {
    let words = emit(&CONDITIONAL, CONDITIONAL.len() - 1)?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(path, &bytes)?;
    println!("PASS: emitted {} SPIR-V words from {} conditional SSA nodes", words.len(), CONDITIONAL.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_ssa() {
        assert!(emit(&[Op::Index, Op::SelectUlt(0,0,0,1)], 1).is_err());
        assert!(emit(&[Op::Index, Op::Shl(0,1)], 1).is_err());
        assert!(emit(&[Op::Xor(0,0)], 0).is_err());
        assert!(emit(&[Op::Constant(3), Op::Sub(0,1)], 1).is_err());
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

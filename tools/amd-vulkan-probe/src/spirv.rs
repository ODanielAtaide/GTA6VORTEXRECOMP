// SPDX-License-Identifier: GPL-3.0-or-later
//! Original synthetic workloads and independent CPU reference functions.
use std::path::Path;
pub use vortex_shader::spirv::{Op, emit};

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

// Exact binary fractions over the bounded invocation domain; no NaNs,
// denormals or rounding-mode assumptions are exercised by this test.
pub const FLOAT: [Op; 8] = [
    Op::Index, Op::UToF(0), Op::Constant(0x3f000000), Op::FMul(1,2),
    Op::Constant(0x3fa00000), Op::FAdd(3,4),
    Op::Constant(0x3e800000), Op::FSub(5,6),
];

pub fn float_expected(index: u32) -> u32 {
    ((index as f32) * 0.5 + 1.25 - 0.25).to_bits()
}

pub fn write_float(path: &Path) -> crate::Result<()> {
    let words = emit(&FLOAT, FLOAT.len()-1)?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(path, &bytes)?;
    println!("PASS: emitted {} SPIR-V words from {} FP32 SSA nodes", words.len(), FLOAT.len());
    Ok(())
}

// Two consecutive structured selections, with divergent predicates and
// a value produced by the first merge consumed by the second.
pub const BRANCH: [Op; 9] = [
    Op::Index, Op::Constant(1), Op::And(0,1), Op::Constant(0xf1234567),
    Op::Xor(0,3), Op::BranchAddSub(2,1,4,3),
    Op::Constant(524288), Op::BranchAddSub(0,6,5,1), Op::Xor(7,0),
];

pub fn branch_expected(index: u32) -> u32 {
    let x = index ^ 0xf1234567;
    let y = if index & 1 == 0 { x.wrapping_add(0xf1234567) }
        else { x.wrapping_sub(0xf1234567) };
    let z = if index < 524288 { y.wrapping_add(1) } else { y.wrapping_sub(1) };
    z ^ index
}

pub fn write_branch(path: &Path) -> crate::Result<()> {
    let words = emit(&BRANCH, BRANCH.len()-1)?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(path, &bytes)?;
    println!("PASS: emitted {} SPIR-V words from {} structured-branch SSA nodes", words.len(), BRANCH.len());
    Ok(())
}

// Two bounded loops with divergent iteration counts and dependent results.
pub const LOOP: [Op; 8] = [
    Op::Index, Op::Constant(0xdeadbeef), Op::Xor(0,1),
    Op::Constant(3), Op::LoopFold(2,0,3), Op::Constant(5),
    Op::LoopFold(4,4,5), Op::Xor(6,0),
];

pub fn loop_expected(index: u32) -> u32 {
    let mut acc = index ^ 0xdeadbeef;
    for n in 0..(index & 31) { acc = acc.wrapping_mul(3).wrapping_add(n); }
    let count = acc & 31;
    for n in 0..count { acc = acc.wrapping_mul(5).wrapping_add(n); }
    acc ^ index
}

pub fn write_loop(path: &Path) -> crate::Result<()> {
    let words = emit(&LOOP, LOOP.len()-1)?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(path, &bytes)?;
    println!("PASS: emitted {} SPIR-V words from {} loop SSA nodes", words.len(), LOOP.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_ssa() {
        assert!(emit(&[Op::Index, Op::LoopFold(0,0,1)], 1).is_err());
        assert!(emit(&[Op::Index, Op::BranchAddSub(0,0,0,1)], 1).is_err());
        assert!(emit(&[Op::UToF(0)], 0).is_err());
        assert!(emit(&[Op::Index, Op::FAdd(0,1)], 1).is_err());
        assert!(emit(&[Op::Index, Op::SelectUlt(0,0,0,1)], 1).is_err());
        assert!(emit(&[Op::Index, Op::Shl(0,1)], 1).is_err());
        assert!(emit(&[Op::Xor(0,0)], 0).is_err());
        assert!(emit(&[Op::Constant(3), Op::Sub(0,1)], 1).is_err());
        assert!(emit(&[], 0).is_err());
        assert!(emit(&[Op::Add(0,0)], 0).is_err());
        assert!(emit(&[Op::Index], 1).is_err());
    }
    #[test]
    fn exact_float_reference() {
        assert_eq!(float_expected(0), 1.0f32.to_bits());
        assert_eq!(float_expected(3), 2.5f32.to_bits());
        assert_eq!(float_expected(1_048_575), 524288.5f32.to_bits());
        assert!(emit(&FLOAT, FLOAT.len()-1).is_ok());
    }
    #[test]
    fn structured_branch_has_two_merges_and_phis() {
        let module = emit(&BRANCH, BRANCH.len()-1).unwrap();
        let mut at = 5;
        let mut counts = [0; 3];
        while at < module.len() {
            match module[at] & 0xffff {
                247 => counts[0] += 1,
                250 => counts[1] += 1,
                245 => counts[2] += 1,
                _ => {}
            }
            let n = (module[at] >> 16) as usize;
            assert!(n > 0 && at + n <= module.len());
            at += n;
        }
        assert_eq!(counts, [2,2,2]);
    }
    #[test]
    fn loop_has_two_merges_and_four_phis() {
        let module = emit(&LOOP, LOOP.len()-1).unwrap();
        let mut at = 5;
        let mut counts = [0; 2];
        while at < module.len() {
            match module[at] & 0xffff {
                246 => counts[0] += 1,
                245 => counts[1] += 1,
                _ => {}
            }
            let n = (module[at] >> 16) as usize;
            assert!(n > 0 && at+n <= module.len());
            at += n;
        }
        assert_eq!(counts, [2,4]);
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

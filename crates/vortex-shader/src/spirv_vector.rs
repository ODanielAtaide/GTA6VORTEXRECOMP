//! Experimental straight-line vector arithmetic diagnostic.
//! The fixture defines logical lane = GlobalInvocationId.x % wave_size;
//! it does not assume that the physical Vulkan subgroup has this lane order.
//! No EXEC writes, cross-lane instructions, memory operations or modifiers.
use crate::{ShaderError, gfx10::{Encoding, Fields, Operand}, ir::{self, Reg, Value, EXEC}, spirv::{self, Op}};
use std::collections::BTreeMap;

const LO: u32 = 0x5555_5555;
const HI: u32 = 0xaaaa_aaaa;
const OLD_V1: u32 = 0x1234_5678;
const OLD_V2: u32 = 0xcafe_babe;

fn push(ops: &mut Vec<Op>, op: Op) -> usize {
    let i = ops.len(); ops.push(op); i
}

/// Diagnostic inputs: v0 = invocation index, v1/v2 = sentinels,
/// EXEC = fixed alternating mask. All other live inputs are rejected.
pub fn emit_vector(program: &ir::Program, output: Reg) -> Result<Vec<u32>, ShaderError> {
    let unsupported = |offset, what| ShaderError::Unsupported { offset, what };
    if program.blocks.len() != 1 || !matches!(output, Reg::V(0..=2)) {
        return Err(unsupported(0, "vector diagnostic requires one block and v0-v2 output"));
    }
    let block = &program.blocks[0];
    if !block.phis.is_empty() || !block.preds.is_empty() || block.exit != ir::Exit::End {
        return Err(unsupported(block.offset, "vector diagnostic control flow"));
    }
    let mut ops = Vec::new();
    let idx = push(&mut ops, Op::Index);
    let zero = push(&mut ops, Op::Constant(0));
    let one = push(&mut ops, Op::Constant(1));
    let mask = push(&mut ops, Op::Constant(31));
    let lane = push(&mut ops, Op::And(idx,mask));
    let lo = push(&mut ops, Op::Constant(LO));
    let hi = push(&mut ops, Op::Constant(HI));
    let wave_mask = if program.wave64 {
        let m = push(&mut ops, Op::Constant(32));
        let half = push(&mut ops, Op::And(idx,m));
        push(&mut ops, Op::SelectUlt(zero,half,hi,lo))
    } else { lo };
    let shifted = push(&mut ops, Op::Shr(wave_mask,lane));
    let active = push(&mut ops, Op::And(shifted,one));
    let mut values = BTreeMap::<Value,usize>::new();
    let mut regs = BTreeMap::<Reg,usize>::new();
    let mut exec_inputs = BTreeMap::<Reg,Value>::new();
    for &(reg, value) in &program.inputs {
        let id = match reg {
            Reg::V(0) => idx,
            Reg::V(1) => push(&mut ops, Op::Constant(OLD_V1)),
            Reg::V(2) => push(&mut ops, Op::Constant(OLD_V2)),
            Reg::S(EXEC) => { exec_inputs.insert(reg,value); continue; }
            Reg::S(r) if program.wave64 && r == EXEC+1 => { exec_inputs.insert(reg,value); continue; }
            _ => return Err(unsupported(0, "unsupported vector diagnostic input")),
        };
        if values.insert(value,id).is_some() { return Err(unsupported(0,"duplicate SSA input")); }
        regs.insert(reg,id);
    }
    for inst in &block.insts {
        let reject = || unsupported(inst.machine.offset, "unsupported vector instruction or modifier");
        if inst.name() == "s_endpgm" && inst.reads.is_empty() && inst.writes.is_empty() { continue; }
        let Fields::Valu(v) = &inst.fields else { return Err(reject()); };
        if !matches!(inst.machine.encoding, Encoding::Vop1 | Encoding::Vop2)
            || v.carry.is_some() || v.neg != 0 || v.abs != 0 || v.neg_hi != 0
            || v.opsel != 0 || v.opsel_hi != 0 || v.clamp || v.omod != 0
            || v.sdwa.is_some() || v.dpp.is_some()
        { return Err(reject()); }
        for r in [Reg::S(EXEC), Reg::S(EXEC+1)].into_iter().take(if program.wave64 {2} else {1}) {
            if exec_inputs.get(&r).copied().is_none() || inst.read(r) != exec_inputs.get(&r).copied() { return Err(reject()); }
        }
        if inst.reads.iter().any(|(r,_)| !matches!(r, Reg::V(0..=2)) && !exec_inputs.contains_key(r)) { return Err(reject()); }
        let Some(Operand::Vector { reg: dst @ 0..=2, dwords: 1 }) = v.dst else { return Err(reject()); };
        let dest = Reg::V(dst);
        let old = *values.get(&inst.read(dest).ok_or_else(reject)?).ok_or_else(reject)?;
        let mut args = Vec::new();
        for source in &v.src {
            args.push(match *source {
                Operand::Vector { reg: r @ 0..=2, dwords: 1 } =>
                    *values.get(&inst.read(Reg::V(r)).ok_or_else(reject)?).ok_or_else(reject)?,
                Operand::Literal(n) => push(&mut ops, Op::Constant(n)),
                Operand::Const(n @ 128..=192) => push(&mut ops, Op::Constant(u32::from(n-128))),
                Operand::Const(n @ 193..=208) => push(&mut ops, Op::Constant(0u32.wrapping_sub(u32::from(n-192)))),
                _ => return Err(reject()),
            });
        }
        let computed = match (inst.name(), args.as_slice()) {
            ("v_mov_b32", &[a]) => a,
            ("v_add_nc_u32", &[a,b]) => push(&mut ops, Op::Add(a,b)),
            ("v_sub_nc_u32", &[a,b]) => push(&mut ops, Op::Sub(a,b)),
            ("v_and_b32", &[a,b]) => push(&mut ops, Op::And(a,b)),
            ("v_or_b32", &[a,b]) => push(&mut ops, Op::Or(a,b)),
            ("v_xor_b32", &[a,b]) => push(&mut ops, Op::Xor(a,b)),
            ("v_lshlrev_b32", &[a,b]) => push(&mut ops, Op::Shl(b,a)),
            ("v_lshrrev_b32", &[a,b]) => push(&mut ops, Op::Shr(b,a)),
            _ => return Err(reject()),
        };
        if inst.writes.len() != 1 { return Err(reject()); }
        let value = inst.write(dest).ok_or_else(reject)?;
        let merged = push(&mut ops, Op::SelectUlt(zero,active,computed,old));
        if values.insert(value,merged).is_some() { return Err(reject()); }
        regs.insert(dest,merged);
        if ops.len() > 4096 { return Err(reject()); }
    }
    spirv::emit(&ops,*regs.get(&output).ok_or_else(|| unsupported(0,"undefined vector diagnostic output"))?)
}

pub fn demo(wave64: bool) -> Result<Vec<u32>, ShaderError> {
    // v1=v0; v1=7+v1; v2=3^v1; v2=v2<<1; v2=64-v2.
    let code: Vec<u8> = [0x7e02_0300u32,0x4a02_0287,0x3a04_0283,0x3404_0481,0x4c04_04c0,0xbf81_0000]
        .into_iter().flat_map(u32::to_le_bytes).collect();
    emit_vector(&ir::build(&code,wave64,3)?,Reg::V(2))
}

pub fn expected32(index: u32) -> u32 {
    if index & 1 == 0 { 64u32.wrapping_sub(((index+7)^3)<<1) } else { OLD_V2 }
}

pub fn expected64(index: u32) -> u32 {
    if (index & 1 == 0) == (index & 32 == 0) {
        64u32.wrapping_sub(((index+7)^3)<<1)
    } else { OLD_V2 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoded_vector_fixtures_emit() {
        for wave64 in [false,true] { assert_eq!(demo(wave64).unwrap()[0],0x07230203); }
    }
    #[test]
    fn inactive_lane_preserves_sentinel() {
        assert_eq!(expected32(1), OLD_V2);
        assert_eq!(expected64(32), OLD_V2);
        assert_ne!(expected64(33), OLD_V2);
    }
}

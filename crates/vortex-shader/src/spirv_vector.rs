//! Experimental straight-line vector arithmetic diagnostic.
//! The fixture defines logical lane = GlobalInvocationId.x % wave_size;
//! it does not assume that the physical Vulkan subgroup has this lane order.
//! Supports immediate EXEC writes and per-lane unsigned CMPX predicates.
//! EXEC SSA values represent only this logical lane's bit, not whole masks.
//! No cross-lane instructions,
//! memory operations or modifiers.
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
/// EXEC starts with an alternating mask. Immediate writes and CMPX may change it.
/// All other live inputs are rejected.
pub fn emit_vector(program: &ir::Program, output: Reg) -> Result<Vec<u32>, ShaderError> {
    emit_with_inputs(program, Some(output), false)
}

/// Explicit in-place kernel ABI: v0 = global invocation index, v1 and v2
/// = initial buffer word for that invocation, s0/s1 = uniform push constants.
/// EXEC initially enables all logical lanes. Only the selected result register
/// is written back, not arbitrary guest addresses. No native memory opcodes.
pub fn emit_buffer_kernel(program: &ir::Program, output: Reg) -> Result<Vec<u32>, ShaderError> {
    emit_with_inputs(program, Some(output), true)
}

/// Restricted native memory kernel: s[4:7] is a host-provided logical
/// descriptor for binding 0:0 (stride 4); v0 is the immutable invocation
/// index. Only indexed dword accesses with zero offsets are supported.
/// Native stores are the only observable output. No cross-invocation access.
pub fn emit_memory_kernel(program: &ir::Program) -> Result<Vec<u32>, ShaderError> {
    emit_with_inputs(program, None, true)
}

fn emit_with_inputs(program: &ir::Program, output: Option<Reg>, buffer_inputs: bool) -> Result<Vec<u32>, ShaderError> {
    let memory = output.is_none();
    let unsupported = |offset, what| ShaderError::Unsupported { offset, what };
    if program.blocks.len() != 1 || output.is_some_and(|r| !matches!(r, Reg::V(0..=2))) {
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
    let lo = push(&mut ops, Op::Constant(if buffer_inputs { u32::MAX } else { LO }));
    let hi = push(&mut ops, Op::Constant(if buffer_inputs { u32::MAX } else { HI }));
    let lo_shifted = push(&mut ops, Op::Shr(lo,lane));
    let hi_shifted = push(&mut ops, Op::Shr(hi,lane));
    let lo_bit = push(&mut ops, Op::And(lo_shifted,one));
    let hi_bit = push(&mut ops, Op::And(hi_shifted,one));
    let half = if program.wave64 {
        let m = push(&mut ops, Op::Constant(32));
        Some(push(&mut ops, Op::And(idx,m)))
    } else { None };
    let mut values = BTreeMap::<Value,usize>::new();
    let mut regs = BTreeMap::<Reg,usize>::new();
    let mut exec_values = BTreeMap::<Reg,Value>::new();
    let mut resource_values = BTreeMap::<Reg,Value>::new();
    for &(reg, value) in &program.inputs {
        if memory && matches!(reg,Reg::S(4..=7)) {
            resource_values.insert(reg,value);
            continue;
        }
        let id = match reg {
            Reg::V(0) => idx,
            Reg::V(1) => push(&mut ops, if memory { Op::Constant(0) } else if buffer_inputs { Op::LoadSelf } else { Op::Constant(OLD_V1) }),
            Reg::V(2) => push(&mut ops, if memory { Op::Constant(0) } else if buffer_inputs { Op::LoadSelf } else { Op::Constant(OLD_V2) }),
            Reg::S(r @ 0..=1) if buffer_inputs => push(&mut ops, Op::Push(r)),
            Reg::S(EXEC) => { exec_values.insert(reg,value); lo_bit }
            Reg::S(r) if program.wave64 && r == EXEC+1 => { exec_values.insert(reg,value); hi_bit }
            _ => return Err(unsupported(0, "unsupported vector diagnostic input")),
        };
        if values.insert(value,id).is_some() { return Err(unsupported(0,"duplicate SSA input")); }
        regs.insert(reg,id);
    }
    for inst in &block.insts {
        let reject = || unsupported(inst.machine.offset, "unsupported vector instruction or modifier");
        if inst.name() == "s_endpgm" && inst.reads.is_empty() && inst.writes.is_empty() { continue; }
        if inst.name() == "s_mov_b32" {
            let Fields::Sop { dst: Some(Operand::Scalar { code, dwords: 1 }), src, imm: None } = &inst.fields else { return Err(reject()); };
            if !(*code == EXEC || (program.wave64 && *code == EXEC+1)) || !inst.reads.is_empty() || inst.writes.len()!=1 { return Err(reject()); }
            let immediate = match src.as_slice() {
                [Operand::Literal(n)] => *n,
                [Operand::Const(n @ 128..=192)] => u32::from(*n-128),
                [Operand::Const(n @ 193..=208)] => 0u32.wrapping_sub(u32::from(*n-192)),
                _ => return Err(reject()),
            };
            let reg = Reg::S(*code);
            let value = inst.write(reg).ok_or_else(reject)?;
            let word = push(&mut ops, Op::Constant(immediate));
            let shifted = push(&mut ops, Op::Shr(word,lane));
            let node = push(&mut ops, Op::And(shifted,one));
            if values.insert(value,node).is_some() { return Err(reject()); }
            exec_values.insert(reg,value);
            if ops.len() > 4096 { return Err(reject()); }
            continue;
        }
        for r in [Reg::S(EXEC), Reg::S(EXEC+1)].into_iter().take(if program.wave64 {2} else {1}) {
            if exec_values.get(&r).copied().is_none() || inst.read(r) != exec_values.get(&r).copied() { return Err(reject()); }
        }
        let lo_value = *values.get(&inst.read(Reg::S(EXEC)).ok_or_else(reject)?).ok_or_else(reject)?;
        let active = if let Some(half) = half {
            let hi_value = *values.get(&inst.read(Reg::S(EXEC+1)).ok_or_else(reject)?).ok_or_else(reject)?;
            push(&mut ops, Op::SelectUlt(zero,half,hi_value,lo_value))
        } else { lo_value };
        if let Fields::Buffer { data,addr,rsrc,soffset,offset,offen,idxen,format,lds,tfe,cache } = &inst.fields {
            if !memory || inst.machine.encoding != Encoding::Mubuf
                || *offen || !*idxen || *offset != 0 || format.is_some() || *lds || *tfe
                || cache.glc || cache.slc || cache.dlc
                || *addr != (Operand::Vector { reg:0,dwords:1 })
                || *rsrc != (Operand::Scalar { code:4,dwords:4 })
                || *soffset != Operand::Const(128) {
                return Err(reject());
            }
            for r in 4..=7 {
                if resource_values.get(&Reg::S(r)).copied().is_none()
                    || inst.read(Reg::S(r)) != resource_values.get(&Reg::S(r)).copied() {
                    return Err(reject());
                }
            }
            let Some(Operand::Vector { reg: r @ 1..=2,dwords:1 }) = *data else { return Err(reject()); };
            let dest = Reg::V(r);
            if inst.reads.iter().any(|(r,_)| *r != dest && *r != Reg::V(0)
                && !resource_values.contains_key(r) && !exec_values.contains_key(r)) {
                return Err(reject());
            }
            if inst.read(Reg::V(0)).and_then(|v| values.get(&v).copied()) != Some(idx) {
                return Err(reject());
            }
            let old = *values.get(&inst.read(dest).ok_or_else(reject)?).ok_or_else(reject)?;
            match inst.name() {
                "buffer_load_dword" => {
                    if inst.writes.len() != 1 { return Err(reject()); }
                    let value = inst.write(dest).ok_or_else(reject)?;
                    // The address is always valid and private to this invocation.
                    // A speculative read of this ordinary buffer is unobservable;
                    // EXEC still gates the architectural VGPR write.
                    let loaded = push(&mut ops,Op::LoadSelf);
                    let merged = push(&mut ops,Op::SelectUlt(zero,active,loaded,old));
                    if values.insert(value,merged).is_some() { return Err(reject()); }
                    regs.insert(dest,merged);
                }
                "buffer_store_dword" => {
                    if !inst.writes.is_empty() { return Err(reject()); }
                    push(&mut ops,Op::StoreSelf(active,old));
                }
                _ => return Err(reject()),
            }
            if ops.len() > 4096 { return Err(reject()); }
            continue;
        }
        let Fields::Valu(v) = &inst.fields else { return Err(reject()); };
        if !matches!(inst.machine.encoding, Encoding::Vop1 | Encoding::Vop2 | Encoding::Vopc)
            || v.carry.is_some() || v.neg != 0 || v.abs != 0 || v.neg_hi != 0
            || v.opsel != 0 || v.opsel_hi != 0 || v.clamp || v.omod != 0
            || v.sdwa.is_some() || v.dpp.is_some()
        { return Err(reject()); }
        if inst.reads.iter().any(|(r,_)| !matches!(r, Reg::V(0..=2))
            && !exec_values.contains_key(r) && !(buffer_inputs && matches!(r, Reg::S(0..=1)))) { return Err(reject()); }
        let mut args = Vec::new();
        for source in &v.src {
            args.push(match *source {
                Operand::Vector { reg: r @ 0..=2, dwords: 1 } =>
                    *values.get(&inst.read(Reg::V(r)).ok_or_else(reject)?).ok_or_else(reject)?,
                Operand::Scalar { code: r @ 0..=1, dwords: 1 } if buffer_inputs =>
                    *values.get(&inst.read(Reg::S(r)).ok_or_else(reject)?).ok_or_else(reject)?,
                Operand::Literal(n) => push(&mut ops, Op::Constant(n)),
                Operand::Const(n @ 128..=192) => push(&mut ops, Op::Constant(u32::from(n-128))),
                Operand::Const(n @ 193..=208) => push(&mut ops, Op::Constant(0u32.wrapping_sub(u32::from(n-192)))),
                _ => return Err(reject()),
            });
        }
        if inst.machine.encoding == Encoding::Vopc {
            let &[a,b] = args.as_slice() else { return Err(reject()); };
            let pred = match inst.name() {
                "v_cmpx_lt_u32" => push(&mut ops, Op::SelectUlt(a,b,one,zero)),
                "v_cmpx_gt_u32" => push(&mut ops, Op::SelectUlt(b,a,one,zero)),
                "v_cmpx_ge_u32" => push(&mut ops, Op::SelectUlt(a,b,zero,one)),
                "v_cmpx_le_u32" => push(&mut ops, Op::SelectUlt(b,a,zero,one)),
                _ => return Err(reject()),
            };
            let next = push(&mut ops, Op::And(active,pred));
            let count = if program.wave64 {2} else {1};
            if inst.writes.len() != count { return Err(reject()); }
            for r in [Reg::S(EXEC),Reg::S(EXEC+1)].into_iter().take(count) {
                let value = inst.write(r).ok_or_else(reject)?;
                if values.insert(value,next).is_some() { return Err(reject()); }
                exec_values.insert(r,value);
            }
            if ops.len()>4096 { return Err(reject()); }
            continue;
        }
        let Some(Operand::Vector { reg: dst @ 0..=2, dwords: 1 }) = v.dst else { return Err(reject()); };
        if memory && dst == 0 { return Err(reject()); }
        let dest = Reg::V(dst);
        let old = *values.get(&inst.read(dest).ok_or_else(reject)?).ok_or_else(reject)?;
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
    if let Some(output) = output {
        spirv::emit(&ops,*regs.get(&output).ok_or_else(|| unsupported(0,"undefined vector diagnostic output"))?)
    } else {
        spirv::emit_effects(&ops)
    }
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

/// v1 receives v0 under the original mask, v2 receives v0 under its
/// complement; after restoring all lanes, add the two vectors.
pub fn exec_demo(wave64: bool) -> Result<Vec<u32>, ShaderError> {
    let mut words = vec![0x7e02_0300u32, 0xbefe_03ff, HI];
    if wave64 { words.extend([0xbeff_03ff, LO]); }
    words.extend([0x7e04_0300, 0xbefe_03c1]);
    if wave64 { words.push(0xbeff_03c1); }
    words.extend([0x4a04_0501, 0xbf81_0000]);
    let code: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    emit_vector(&ir::build(&code,wave64,3)?,Reg::V(2))
}

pub fn exec_expected32(index: u32) -> u32 {
    index.wrapping_add(if index & 1 == 0 { OLD_V2 } else { OLD_V1 })
}

pub fn exec_expected64(index: u32) -> u32 {
    index.wrapping_add(if (index & 1 == 0) == (index & 32 == 0) { OLD_V2 } else { OLD_V1 })
}

/// Filter the initial mask twice, write v2, restore EXEC, increment v2.
pub fn cmpx_demo(wave64: bool) -> Result<Vec<u32>, ShaderError> {
    let mut words = vec![
        0x3602_009fu32, // v_and_b32 v1, 31, v0
        0x7da8_00ff, 524288, // v_cmpx_gt_u32 literal, v0
        0x7da8_0290, // v_cmpx_gt_u32 16, v1
        0x7e04_0300, // v_mov_b32 v2, v0
        0xbefe_03c1, // exec_lo = -1
    ];
    if wave64 { words.push(0xbeff_03c1); }
    words.extend([0x4a04_0481,0xbf81_0000]); // v_add_nc_u32 v2, 1, v2
    let code: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    emit_vector(&ir::build(&code,wave64,3)?,Reg::V(2))
}

fn cmpx_expected(index: u32, active: bool) -> u32 {
    let v = if active && index < 524288 && index & 31 < 16 { index } else { OLD_V2 };
    v.wrapping_add(1)
}

pub fn cmpx_expected32(index: u32) -> u32 {
    cmpx_expected(index,index & 1 == 0)
}

pub fn cmpx_expected64(index: u32) -> u32 {
    cmpx_expected(index,(index & 1 == 0) == (index & 32 == 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_native_memory_effects_emit() {
        let words = [0xe030_2000u32,0x8001_0100,0x4a02_0200,
            0xe070_2000,0x8001_0100,0xbf81_0000];
        let code: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        for wave64 in [false,true] {
            let p = ir::build(&code,wave64,3).unwrap();
            assert!(emit_memory_kernel(&p).is_ok());
            assert!(emit_buffer_kernel(&p,Reg::V(1)).is_err());
        }
    }
    #[test]
    fn explicit_buffer_inputs_emit() {
        // v1 = s0 + v1; v2 = s1 XOR v1.
        let code: Vec<u8> = [0x4a02_0200u32,0x3a04_0201,0xbf81_0000]
            .into_iter().flat_map(u32::to_le_bytes).collect();
        for wave64 in [false,true] {
            let p = ir::build(&code,wave64,3).unwrap();
            assert!(p.inputs.iter().any(|(r,_)| *r == Reg::S(0)));
            assert!(p.inputs.iter().any(|(r,_)| *r == Reg::S(1)));
            assert!(emit_buffer_kernel(&p,Reg::V(2)).is_ok());
            assert!(emit_vector(&p,Reg::V(2)).is_err());
        }
    }
    #[test]
    fn decoded_cmpx_updates_emit() {
        for wave64 in [false,true] { assert_eq!(cmpx_demo(wave64).unwrap()[0],0x07230203); }
    }
    #[test]
    fn decoded_exec_updates_emit() {
        for wave64 in [false,true] { assert_eq!(exec_demo(wave64).unwrap()[0],0x07230203); }
    }
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

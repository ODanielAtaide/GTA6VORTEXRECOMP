//! Experimental bridge for closed, straight-line scalar arithmetic programs.
//! This diagnostic ABI replicates a scalar result to every invocation. It
//! is not the graphics-stage or resource binding ABI of a native shader.
use crate::{ShaderError, gfx10::{Fields, Operand}, ir::{self, Reg, Value}, spirv::{self, Op}};
use std::collections::BTreeMap;

fn push(ops: &mut Vec<Op>, op: Op) -> usize {
    let index = ops.len();
    ops.push(op);
    index
}

/// Lower a closed scalar-only program, selecting the final value of an SGPR.
/// Unsupported effects are rejected rather than silently dropped.
pub fn emit_scalar(program: &ir::Program, output: Reg) -> Result<Vec<u32>, ShaderError> {
    let unsupported = |offset, what| ShaderError::Unsupported { offset, what };
    if program.blocks.len() != 1 || !program.inputs.is_empty()
        || !matches!(output, Reg::S(0..=105) | Reg::Scc)
    {
        return Err(unsupported(0, "SPIR-V scalar diagnostic requires a closed single block"));
    }
    let block = &program.blocks[0];
    if !block.phis.is_empty() || !block.preds.is_empty() || block.exit != ir::Exit::End {
        return Err(unsupported(block.offset, "SPIR-V scalar diagnostic control flow"));
    }
    let mut ops = Vec::new();
    let mut values: BTreeMap<Value, usize> = BTreeMap::new();
    let mut registers: BTreeMap<Reg, usize> = BTreeMap::new();
    for inst in &block.insts {
        let reject = || unsupported(inst.machine.offset, "SPIR-V scalar instruction or operand");
        if inst.name() == "s_endpgm" && inst.reads.is_empty() && inst.writes.is_empty() {
            continue;
        }
        let Fields::Sop { dst: Some(Operand::Scalar { code: dst @ 0..=105, dwords: 1 }), src, imm: None } = &inst.fields else {
            return Err(reject());
        };
        let uses_scc = matches!(inst.name(), "s_addc_u32" | "s_subb_u32" | "s_cselect_b32");
        if inst.reads.iter().any(|(r,_)| !matches!(r, Reg::S(0..=105)) && !(*r == Reg::Scc && uses_scc))
            || inst.read(Reg::Scc).is_some() != uses_scc
        {
            return Err(reject());
        }
        let mut args = Vec::new();
        for source in src {
            let v = match *source {
                Operand::Literal(word) => push(&mut ops, Op::Constant(word)),
                Operand::Const(c @ 128..=192) => push(&mut ops, Op::Constant(u32::from(c - 128))),
                Operand::Const(c @ 193..=208) => push(&mut ops, Op::Constant(0u32.wrapping_sub(u32::from(c - 192)))),
                Operand::Scalar { code: s @ 0..=105, dwords: 1 } => {
                    let value = inst.read(Reg::S(s)).ok_or_else(reject)?;
                    *values.get(&value).ok_or_else(reject)?
                }
                _ => return Err(reject()),
            };
            args.push(v);
        }
        let (result, scc) = match (inst.name(), args.as_slice()) {
            ("s_mov_b32", &[a]) => (a, None),
            ("s_sub_u32", &[a,b]) => {
                let result = push(&mut ops, Op::Sub(a,b));
                let one = push(&mut ops, Op::Constant(1));
                let zero = push(&mut ops, Op::Constant(0));
                let borrow = push(&mut ops, Op::SelectUlt(a,b,one,zero));
                (result, Some(borrow))
            }
            (name @ ("s_addc_u32" | "s_subb_u32"), &[a,b]) => {
                let cin = *values.get(&inst.read(Reg::Scc).ok_or_else(reject)?).ok_or_else(reject)?;
                let one = push(&mut ops, Op::Constant(1));
                let zero = push(&mut ops, Op::Constant(0));
                let (result, c0, c1) = if name == "s_addc_u32" {
                    let partial = push(&mut ops, Op::Add(a,b));
                    let result = push(&mut ops, Op::Add(partial,cin));
                    let c0 = push(&mut ops, Op::SelectUlt(partial,a,one,zero));
                    let c1 = push(&mut ops, Op::SelectUlt(result,partial,one,zero));
                    (result,c0,c1)
                } else {
                    let partial = push(&mut ops, Op::Sub(a,b));
                    let result = push(&mut ops, Op::Sub(partial,cin));
                    let c0 = push(&mut ops, Op::SelectUlt(a,b,one,zero));
                    let c1 = push(&mut ops, Op::SelectUlt(partial,cin,one,zero));
                    (result,c0,c1)
                };
                (result, Some(push(&mut ops, Op::Or(c0,c1))))
            }
            ("s_cselect_b32", &[a,b]) => {
                let cin = *values.get(&inst.read(Reg::Scc).ok_or_else(reject)?).ok_or_else(reject)?;
                let zero = push(&mut ops, Op::Constant(0));
                (push(&mut ops, Op::SelectUlt(zero,cin,a,b)), None)
            }
            ("s_mul_i32", &[a,b]) => (push(&mut ops, Op::Mul(a,b)), None),
            (name @ ("s_lshl_b32" | "s_lshr_b32"), &[a,b]) => {
                let result = push(&mut ops, if name == "s_lshl_b32" { Op::Shl(a,b) } else { Op::Shr(a,b) });
                let one = push(&mut ops, Op::Constant(1));
                let zero = push(&mut ops, Op::Constant(0));
                let flag = push(&mut ops, Op::SelectUlt(zero,result,one,zero));
                (result, Some(flag))
            }
            ("s_add_u32", &[a,b]) => {
                let result = push(&mut ops, Op::Add(a,b));
                let one = push(&mut ops, Op::Constant(1));
                let zero = push(&mut ops, Op::Constant(0));
                let carry = push(&mut ops, Op::SelectUlt(result,a,one,zero));
                (result, Some(carry))
            }
            (name @ ("s_and_b32" | "s_or_b32" | "s_xor_b32"), &[a,b]) => {
                let result = push(&mut ops, match name {
                    "s_and_b32" => Op::And(a,b),
                    "s_or_b32" => Op::Or(a,b),
                    _ => Op::Xor(a,b),
                });
                let one = push(&mut ops, Op::Constant(1));
                let zero = push(&mut ops, Op::Constant(0));
                let flag = push(&mut ops, Op::SelectUlt(zero,result,one,zero));
                (result, Some(flag))
            }
            _ => return Err(reject()),
        };
        let dest = Reg::S(*dst);
        if inst.writes.len() != 1 + usize::from(scc.is_some())
            || inst.write(dest).is_none() || inst.write(Reg::Scc).is_some() != scc.is_some()
        {
            return Err(reject());
        }
        for &(reg, value) in &inst.writes {
            let idx = if reg == dest { result } else if reg == Reg::Scc {
                scc.ok_or_else(reject)?
            } else { return Err(reject()); };
            if values.insert(value, idx).is_some() { return Err(reject()); }
            registers.insert(reg, idx);
        }
        if ops.len() > 4096 { return Err(reject()); }
    }
    let result = *registers.get(&output).ok_or_else(|| unsupported(0, "SPIR-V scalar output undefined"))?;
    spirv::emit(&ops, result)
}

/// Original fixture: s0=5; s1=7; s2=s0+s1; s3=s2 XOR 3.
pub fn demo() -> Result<Vec<u32>, ShaderError> {
    let code: Vec<u8> = [0xbe80_0385u32, 0xbe81_0387, 0x8002_0100, 0x8903_8302, 0xbf81_0000]
        .into_iter().flat_map(u32::to_le_bytes).collect();
    let program = ir::build(&code, false, 0)?;
    emit_scalar(&program, Reg::S(3))
}

/// Original scalar test with carry, borrow, SCC selection, and shifts.
/// s0=-1; s1=s0+1=0 carry; s2=7+2+carry=10;
/// s3=0-1=MAX borrow; s4=10-2-borrow=7; SCC=0;
/// s5=3 if SCC else 7; s6=s5*3=21; s7=s6<<2=84;
/// s8=s7>>1=42.  Each instruction uses the original decoder.
pub fn carry_demo() -> Result<Vec<u32>, ShaderError> {
    let code: Vec<u8> = [
        0xbe80_03c1u32, 0x8001_8100, 0x8202_8287,
        0x8083_8180, 0x8284_8202 | (1 << 23),
        0x8505_8783, 0x9306_8305, 0x8f07_8206,
        0x9008_8107, 0xbf81_0000,
    ].into_iter().flat_map(u32::to_le_bytes).collect();
    let program = ir::build(&code, false, 0)?;
    emit_scalar(&program, Reg::S(8))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carry_fixture_emits() {
        assert_eq!(carry_demo().unwrap()[0], 0x07230203);
    }
    #[test]
    fn decoded_scalar_fixture_emits() {
        let words = demo().unwrap();
        assert_eq!(words[0], 0x07230203);
    }
    #[test]
    fn rejects_open_inputs() {
        let code: Vec<u8> = [0xbe80_0301u32, 0xbf81_0000]
            .into_iter().flat_map(u32::to_le_bytes).collect();
        let program = ir::build(&code, false, 0).unwrap();
        assert!(emit_scalar(&program, Reg::S(0)).is_err());
    }
}

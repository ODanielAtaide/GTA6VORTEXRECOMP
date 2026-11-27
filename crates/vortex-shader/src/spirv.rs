// SPDX-License-Identifier: GPL-3.0-or-later
//! Experimental SPIR-V compute lowering for an independent, bounded SSA subset.
//! This module is not yet a lowering of crate::ir::Program. Its output ABI is
//! one uint per invocation in storage buffer set 0, binding 0.
use crate::ShaderError;

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Index,
    /// Read this invocation's original uint in storage buffer 0:0.
    LoadSelf,
    /// Read a component of a private fixed-stride record (word offset).
    LoadRecord { stride: u8, component: u8 },
    /// Predicated store into the same private record.
    StoreRecord { stride: u8, component: u8, active: usize, value: usize },
    /// Store this invocation's word only when the mask is nonzero.
    StoreSelf(usize, usize),
    /// Read one of the two uniform u32 push constants.
    Push(u8),
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
    // If a < b, x+y; otherwise x-y, merged by OpPhi.
    BranchAddSub(usize, usize, usize, usize),
    // Repeat acc = acc * factor + counter, for count & 31 iterations.
    LoopFold(usize, usize, usize),
    // All SSA values remain 32-bit words; these interpret words as IEEE f32.
    UToF(usize),
    FAdd(usize, usize),
    FSub(usize, usize),
    FMul(usize, usize),
}

fn instruction(out: &mut Vec<u32>, opcode: u16, args: &[u32]) {
    out.push((((args.len() + 1) as u32) << 16) | opcode as u32);
    out.extend_from_slice(args);
}

/// Emit a kernel that writes one u32 per invocation to descriptor 0:0.
/// Dispatch must match the size of the output buffer, local size 64.
/// References must name earlier SSA values.
pub fn emit(ops: &[Op], result: usize) -> Result<Vec<u32>, ShaderError> {
    emit_inner(ops,Some(result))
}

/// Emit explicit memory effects, without an implicit final result store.
pub fn emit_effects(ops:&[Op]) -> Result<Vec<u32>, ShaderError> {
    if !ops.iter().any(|op| matches!(op,Op::StoreSelf(..)|Op::StoreRecord{..})) {
        return Err(ShaderError::Malformed("No explicit storage effects"));
    }
    emit_inner(ops,None)
}

fn emit_inner(ops:&[Op],result:Option<usize>) -> Result<Vec<u32>, ShaderError> {
    if ops.is_empty() || ops.len() > 4096 || result.is_some_and(|r| r >= ops.len()) {
        return Err(ShaderError::Malformed("Invalid IR size or result"));
    }
    let has_push = ops.iter().any(|op| matches!(op, Op::Push(_)));
    if ops.iter().any(|op| matches!(op, Op::Push(n) if *n > 1)) {
        return Err(ShaderError::Malformed("Invalid push constant index"));
    }
    for (i, op) in ops.iter().enumerate() {
        match *op {
            Op::LoadRecord { stride,component } | Op::StoreRecord { stride,component,.. }
                if !(1..=16).contains(&stride) || component>=stride =>
                    return Err(ShaderError::Malformed("Invalid record component")),
            Op::StoreRecord { active,value,.. } if active>=i || value>=i =>
                return Err(ShaderError::Malformed("Invalid record store operand")),
            _ => {}
        }
        if let Op::Add(a, b) | Op::Mul(a, b) | Op::Sub(a, b) | Op::And(a, b) | Op::Or(a, b) | Op::Xor(a, b) | Op::Shl(a, b) | Op::Shr(a, b) | Op::FAdd(a,b) | Op::FSub(a,b) | Op::FMul(a,b) | Op::StoreSelf(a,b) = *op {
            if a >= i || b >= i { return Err(ShaderError::Malformed("Invalid forward SSA reference")); }
        }
        if let Op::LoopFold(seed,count,factor) = *op {
            if [seed,count,factor].iter().any(|&v| v >= i) { return Err(ShaderError::Malformed("Invalid loop SSA reference")); }
        }
        if let Op::UToF(a) = *op {
            if a >= i { return Err(ShaderError::Malformed("Invalid conversion SSA reference")); }
        }
        if let Op::SelectUlt(a,b,t,f) | Op::BranchAddSub(a,b,t,f) = *op {
            if [a,b,t,f].iter().any(|&v| v >= i) { return Err(ShaderError::Malformed("Invalid select SSA reference")); }
        }
    }
    // IDs 17..20 are bool type, shift mask, f32 type and constant one.
    // IDs 21..24 are push block type, block pointer, uint pointer, global.
    // IDs 25..41 hold u32 constants 0..16 for record indexing.
    // Each node reserves a result plus twelve temporary/label IDs.
    let id = |i: usize| 42 + 13 * i as u32;
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
    if has_push {
        op(&mut out, 71, &[21, 2]); // Block
        op(&mut out, 72, &[21, 0, 35, 0]); // member 0 offset
        op(&mut out, 72, &[21, 1, 35, 4]); // member 1 offset
    }
    op(&mut out, 19, &[1]); // void
    op(&mut out, 33, &[2, 1]); // function type
    op(&mut out, 21, &[3, 32, 0]); // uint
    op(&mut out, 20, &[17]); // bool
    op(&mut out, 22, &[19, 32]); // f32
    op(&mut out, 23, &[4, 3, 3]); // uvec3
    op(&mut out, 32, &[5, 1, 4]); // Input pointer
    op(&mut out, 29, &[6, 3]); // runtime array
    op(&mut out, 30, &[7, 6]); // block struct
    op(&mut out, 32, &[8, 12, 7]); // StorageBuffer block pointer
    op(&mut out, 32, &[9, 12, 3]); // StorageBuffer uint pointer
    if has_push {
        op(&mut out, 30, &[21, 3, 3]); // Push constant struct
        op(&mut out, 32, &[22, 9, 21]); // PushConstant struct pointer
        op(&mut out, 32, &[23, 9, 3]); // PushConstant uint pointer
    }
    op(&mut out, 59, &[5, 10, 1]); // Input global
    op(&mut out, 59, &[8, 11, 12]); // StorageBuffer global
    if has_push { op(&mut out, 59, &[22, 24, 9]); }
    op(&mut out, 43, &[3, 13, 0]); // member index zero
    op(&mut out, 43, &[3, 18, 31]); // defined modulo-32 shift count
    op(&mut out, 43, &[3, 20, 1]); // loop increment
    if ops.iter().any(|o| matches!(o,Op::LoadRecord{..}|Op::StoreRecord{..})) {
        for n in 0..=16 { op(&mut out,43,&[3,25+n,n]); }
    }
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
            Op::LoadSelf => {
                op(&mut out, 65, &[9, id(i)+1, 11, 13, 16]);
                op(&mut out, 61, &[3, id(i), id(i)+1]);
            }
            Op::LoadRecord { stride,component } => {
                let r = id(i);
                op(&mut out,132,&[3,r+1,16,25+u32::from(stride)]);
                op(&mut out,128,&[3,r+2,r+1,25+u32::from(component)]);
                op(&mut out,65,&[9,r+3,11,13,r+2]);
                op(&mut out,61,&[3,r,r+3]);
            }
            Op::StoreRecord { stride,component,active,value } => {
                let r = id(i);
                op(&mut out,176,&[17,r+1,13,id(active)]);
                op(&mut out,247,&[r+3,0]);
                op(&mut out,250,&[r+1,r+2,r+3]);
                op(&mut out,248,&[r+2]);
                op(&mut out,132,&[3,r+4,16,25+u32::from(stride)]);
                op(&mut out,128,&[3,r+5,r+4,25+u32::from(component)]);
                op(&mut out,65,&[9,r+6,11,13,r+5]);
                op(&mut out,62,&[r+6,id(value)]);
                op(&mut out,249,&[r+3]);
                op(&mut out,248,&[r+3]);
                op(&mut out,83,&[3,r,id(value)]);
            }
            Op::StoreSelf(active,value) => {
                let r = id(i);
                // Structured conditional store; inactive lanes issue no write.
                op(&mut out, 176, &[17,r+1,13,id(active)]);
                op(&mut out, 247, &[r+3,0]);
                op(&mut out, 250, &[r+1,r+2,r+3]);
                op(&mut out, 248, &[r+2]);
                op(&mut out, 65, &[9,r+4,11,13,16]);
                op(&mut out, 62, &[r+4,id(value)]);
                op(&mut out, 249, &[r+3]);
                op(&mut out, 248, &[r+3]);
                op(&mut out, 83, &[3,r,id(value)]);
            }
            Op::Push(n) => {
                let member = if n == 0 { 13 } else { 20 };
                op(&mut out, 65, &[23, id(i)+1, 24, member]);
                op(&mut out, 61, &[3, id(i), id(i)+1]);
            }
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
            Op::UToF(a) => {
                op(&mut out, 112, &[19, id(i)+1, id(a)]); // ConvertUToF
                op(&mut out, 124, &[3, id(i), id(i)+1]); // Bitcast
            }
            Op::FAdd(a,b) | Op::FSub(a,b) | Op::FMul(a,b) => {
                let tmp = id(i)+1;
                op(&mut out, 124, &[19, tmp, id(a)]);
                op(&mut out, 124, &[19, tmp+1, id(b)]);
                let opcode = match node { Op::FAdd(..) => 129, Op::FSub(..) => 131, _ => 133 };
                op(&mut out, opcode, &[19, tmp+2, tmp, tmp+1]);
                op(&mut out, 124, &[3, id(i), tmp+2]);
            }
            Op::LoopFold(seed,count,factor) => {
                let r = id(i);
                let pre = r+1;
                let header = r+2;
                let body = r+3;
                let cont = r+4;
                let merge = r+5;
                let counter = r+6;
                let acc = r+7;
                let cond = r+8;
                let next_counter = r+9;
                let product = r+10;
                let next_acc = r+11;
                let bound = r+12;
                op(&mut out, 199, &[3, bound, id(count), 18]);
                op(&mut out, 249, &[pre]);
                op(&mut out, 248, &[pre]);
                op(&mut out, 249, &[header]);
                op(&mut out, 248, &[header]);
                op(&mut out, 245, &[3, counter, 13, pre, next_counter, cont]);
                op(&mut out, 245, &[3, acc, id(seed), pre, next_acc, cont]);
                op(&mut out, 176, &[17, cond, counter, bound]);
                op(&mut out, 246, &[merge, cont, 0]); // LoopMerge
                op(&mut out, 250, &[cond, body, merge]);
                op(&mut out, 248, &[body]);
                op(&mut out, 132, &[3, product, acc, id(factor)]);
                op(&mut out, 128, &[3, next_acc, product, counter]);
                op(&mut out, 249, &[cont]);
                op(&mut out, 248, &[cont]);
                op(&mut out, 128, &[3, next_counter, counter, 20]);
                op(&mut out, 249, &[header]);
                op(&mut out, 248, &[merge]);
                op(&mut out, 83, &[3, r, acc]);
            }
            Op::BranchAddSub(a,b,x,y) => {
                let r = id(i);
                let cond = r+1;
                let yes = r+2;
                let no = r+3;
                let merge = r+4;
                let sum = r+5;
                let diff = r+6;
                op(&mut out, 176, &[17, cond, id(a), id(b)]);
                op(&mut out, 247, &[merge, 0]); // SelectionMerge
                op(&mut out, 250, &[cond, yes, no]); // BranchConditional
                op(&mut out, 248, &[yes]);
                op(&mut out, 128, &[3, sum, id(x), id(y)]);
                op(&mut out, 249, &[merge]);
                op(&mut out, 248, &[no]);
                op(&mut out, 130, &[3, diff, id(x), id(y)]);
                op(&mut out, 249, &[merge]);
                op(&mut out, 248, &[merge]);
                op(&mut out, 245, &[3, r, sum, yes, diff, no]); // Phi
            }
            Op::SelectUlt(a,b,t,f) => {
                let tmp = id(i) + 1;
                op(&mut out, 176, &[17, tmp, id(a), id(b)]);
                op(&mut out, 169, &[3, id(i), tmp, id(t), id(f)]);
            }
        }
    }
    if let Some(result) = result {
        op(&mut out, 65, &[9, ptr, 11, 13, 16]); // AccessChain
        op(&mut out, 62, &[ptr, id(result)]); // Store
    }
    op(&mut out, 253, &[]); // Return
    op(&mut out, 56, &[]); // FunctionEnd
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_effects_validate() {
        assert!(emit_effects(&[Op::Constant(1),Op::LoadRecord{stride:4,component:3},
            Op::StoreRecord{stride:4,component:3,active:0,value:1}]).is_ok());
        assert!(emit(&[Op::LoadRecord{stride:4,component:4}],0).is_err());
    }

    #[test]
    fn explicit_store_requires_valid_operands() {
        assert!(emit_effects(&[Op::Constant(1),Op::LoadSelf,Op::StoreSelf(0,1)]).is_ok());
        assert!(emit_effects(&[Op::LoadSelf]).is_err());
        assert!(emit_effects(&[Op::StoreSelf(0,0)]).is_err());
    }
    #[test]
    fn buffer_and_push_inputs_emit() {
        assert!(emit(&[Op::LoadSelf,Op::Push(0),Op::Add(0,1)],2).is_ok());
        assert!(emit(&[Op::Push(2)],0).is_err());
    }
    #[test]
    fn rejects_forward_reference() {
        assert!(matches!(emit(&[Op::Add(0,0)], 0), Err(ShaderError::Malformed(_))));
    }
    #[test]
    fn emits_spirv_compute_header() {
        let words = emit(&[Op::Index, Op::Constant(2), Op::Mul(0,1)], 2).unwrap();
        assert_eq!(&words[..3], &[0x07230203, 0x00010300, 0]);
        assert!(words.len() > 5);
    }
}

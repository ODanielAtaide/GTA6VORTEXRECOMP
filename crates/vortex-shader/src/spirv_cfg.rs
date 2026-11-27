// SPDX-License-Identifier: GPL-3.0-or-later
//! Experimental native compute CFG backend. Unlike the straight-line diagnostic,
//! this lowers actual decoded block edges, scalar branches and loops. Register
//! state persists across edges, so phi joins are represented by mutable slots.
//! ABI: v0 = global index (immutable), v1 = local index, v2 = physical subgroup
//! size; other VGPRs initially zero; s0/s1 = push constants; s[4:7] = storage
//! descriptor 0:0 with stride four. EXEC is always full, never modified here.
//! Native memory effects only; no implicit output. One-dimensional workgroups of
//! 64. Dynamic dword indices are bounds checked. Ordinary stores may only use v0
//! and zero offset. Atomic accesses may share addresses. The caller must ensure
//! ordinary reads and LDS accesses are race free. No guest pointers, indirect
//! jumps, divergent control, arbitrary descriptors or general memory model yet.
//! Execution is capped at 4096 basic-block visits to avoid unbounded GPU hangs.
use std::collections::BTreeMap;
use crate::{ShaderError, gfx10::{Fields, Operand, Encoding}, ir::{self, Reg, Exit, EXEC}};

const UINT:u32=3;
const BOOL:u32=4;
const GLOBAL:u32=10;
const STORAGE:u32=11;
const PUSH:u32=12;
const LDS:u32=13;
const SUBGROUP:u32=14;
const FUNCTION_PTR:u32=15;
const STORAGE_PTR:u32=16;
const PUSH_PTR:u32=17;
const LDS_PTR:u32=18;
const MAIN:u32=19;

fn inst(out:&mut Vec<u32>,opcode:u16,args:&[u32]) {
    out.push(((args.len() as u32+1)<<16)|u32::from(opcode));
    out.extend_from_slice(args);
}
fn unsupported(offset:usize,what:&'static str)->ShaderError {
    ShaderError::Unsupported { offset,what }
}
struct Emit {
    next:u32, globals:Vec<u32>, body:Vec<u32>, constants:BTreeMap<u32,u32>,
    regs:BTreeMap<Reg,u32>, pc:u32,budget:u32, global_index:u32, length:u32,
}
impl Emit {
    fn id(&mut self)->u32 { let n=self.next; self.next+=1; n }
    fn op(&mut self,code:u16,args:&[u32]) { inst(&mut self.body,code,args); }
    fn c(&mut self,n:u32)->u32 {
        if let Some(&id)=self.constants.get(&n) { return id; }
        let id=self.id(); self.constants.insert(n,id);
        inst(&mut self.globals,43,&[UINT,id,n]); id
    }
    fn value(&mut self,code:u16,ty:u32,args:&[u32])->u32 {
        let id=self.id(); let mut a=vec![ty,id]; a.extend_from_slice(args);
        self.op(code,&a); id
    }
    fn u(&mut self,code:u16,a:u32,b:u32)->u32 { self.value(code,UINT,&[a,b]) }
    fn test(&mut self,code:u16,a:u32,b:u32)->u32 { self.value(code,BOOL,&[a,b]) }
    fn select(&mut self,condition:u32,a:u32,b:u32)->u32 { self.value(169,UINT,&[condition,a,b]) }
    fn flag(&mut self,condition:u32)->u32 { let a=self.c(1);let b=self.c(0);self.select(condition,a,b) }
    fn load(&mut self,p:u32)->u32 { self.value(61,UINT,&[p]) }
    fn reg(&mut self,r:Reg)->Result<u32,ShaderError> {
        let p=*self.regs.get(&r).ok_or(unsupported(0,"CFG register"))?; Ok(self.load(p))
    }
    fn put(&mut self,r:Reg,v:u32)->Result<(),ShaderError> {
        let p=*self.regs.get(&r).ok_or(unsupported(0,"CFG destination"))?;
        self.op(62,&[p,v]); Ok(())
    }
    fn operand(&mut self,o:Operand)->Result<u32,ShaderError> {
        match o {
            Operand::Literal(n)=>Ok(self.c(n)),
            Operand::Const(n @ 128..=192)=>Ok(self.c(u32::from(n-128))),
            Operand::Const(n @ 193..=208)=>Ok(self.c(0u32.wrapping_sub(u32::from(n-192)))),
            Operand::Scalar{code,dwords:1} if scalar(code)=>self.reg(Reg::S(code)),
            Operand::Vector{reg,dwords:1} if reg<=31=>self.reg(Reg::V(reg)),
            _=>Err(unsupported(0,"CFG operand")),
        }
    }
    fn pointer(&mut self,ty:u32,base:u32,indices:&[u32])->u32 {
        let mut a=vec![base];a.extend_from_slice(indices); self.value(65,ty,&a)
    }
    fn guard(&mut self,condition:u32)->u32 {
        let yes=self.id();let merge=self.id();
        self.op(247,&[merge,0]);self.op(250,&[condition,yes,merge]);self.op(248,&[yes]);merge
    }
    fn end_guard(&mut self,merge:u32) { self.op(249,&[merge]);self.op(248,&[merge]); }
}
fn scalar(r:u8)->bool { r<=105 && !(4..=7).contains(&r) }
fn ordinary(r:Reg)->bool { matches!(r,Reg::S(n) if scalar(n)) || matches!(r,Reg::Scc|Reg::V(0..=31)) }
fn descriptor(r:Reg)->bool { matches!(r,Reg::S(4..=7)) }
fn exec(r:Reg)->bool { matches!(r,Reg::S(n) if n==EXEC||n==EXEC+1) }
fn writes(inst:&ir::Inst,expected:&[Reg])->bool {
    inst.writes.len()==expected.len() && expected.iter().all(|r|inst.write(*r).is_some())
}
fn vector(o:Operand)->Option<u8> { match o { Operand::Vector{reg,dwords:1} if reg<=31=>Some(reg),_=>None } }

fn lower(e:&mut Emit,i:&ir::Inst)->Result<(),ShaderError> {
    let err=||unsupported(i.machine.offset,"native CFG instruction or operand");
    let name=i.name();
    if i.reads.iter().any(|(r,_)|!ordinary(*r)&&!descriptor(*r)&&!exec(*r))
        || i.writes.iter().any(|(r,_)|!ordinary(*r)||*r==Reg::V(0)) { return Err(err()); }
    match &i.fields {
        Fields::Sopp{..} => match name {
            "s_branch"|"s_cbranch_scc0"|"s_cbranch_scc1"|"s_endpgm" if i.writes.is_empty()=>Ok(()),
            "s_barrier" if i.writes.is_empty() => {
                // All control in this backend is scalar and workgroup-uniform.
                let scope=e.c(2);let semantics=e.c(0x108);
                e.op(224,&[scope,scope,semantics]); Ok(())
            }
            _=>Err(err()),
        },
        Fields::Sop{dst,src,imm:None}=>{
            if src.iter().any(|o|matches!(o,Operand::Vector{..})) { return Err(err()); }
            let args:Vec<u32>=src.iter().map(|o|e.operand(*o)).collect::<Result<_,_>>()?;
            if dst.is_none() {
                let &[a,b]=args.as_slice() else {return Err(err());};
                let opcode=match name {
                    "s_cmp_eq_u32"=>170,"s_cmp_lg_u32"=>171,
                    "s_cmp_gt_u32"=>172,"s_cmp_ge_u32"=>174,
                    "s_cmp_lt_u32"=>176,"s_cmp_le_u32"=>178,_=>return Err(err()),
                };
                if !writes(i,&[Reg::Scc]) {return Err(err());}
                let cond=e.test(opcode,a,b);let v=e.flag(cond);return e.put(Reg::Scc,v);
            }
            let Some(Operand::Scalar{code,dwords:1})=*dst else {return Err(err());};
            if !scalar(code) {return Err(err());}
            let dest=Reg::S(code);
            let (v,flag)=match (name,args.as_slice()) {
                ("s_mov_b32",&[a])=>(a,None),
                ("s_add_u32",&[a,b])=>{let v=e.u(128,a,b);let c=e.test(176,v,a);(v,Some(e.flag(c)))},
                ("s_sub_u32",&[a,b])=>{let v=e.u(130,a,b);let c=e.test(176,a,b);(v,Some(e.flag(c)))},
                ("s_mul_i32",&[a,b])=>(e.u(132,a,b),None),
                ("s_cselect_b32",&[a,b])=>{let c=e.reg(Reg::Scc)?;let z=e.c(0);let t=e.test(171,c,z);(e.select(t,a,b),None)},
                ("s_and_b32"|"s_or_b32"|"s_xor_b32"|"s_lshl_b32"|"s_lshr_b32",&[a,b])=>{
                    let v=match name {
                        "s_and_b32"=>e.u(199,a,b),"s_or_b32"=>e.u(197,a,b),"s_xor_b32"=>e.u(198,a,b),
                        _=>{let mask=e.c(31);let n=e.u(199,b,mask);e.u(if name=="s_lshl_b32"{196}else{194},a,n)}
                    };
                    let z=e.c(0);let c=e.test(171,v,z);(v,Some(e.flag(c)))
                }
                _=>return Err(err()),
            };
            let mut expected=vec![dest];if flag.is_some(){expected.push(Reg::Scc);}
            if !writes(i,&expected){return Err(err());}
            e.put(dest,v)?;if let Some(c)=flag{e.put(Reg::Scc,c)?;} Ok(())
        }
        Fields::Valu(v)=>{
            if !matches!(i.machine.encoding,Encoding::Vop1|Encoding::Vop2)
                || v.carry.is_some() || v.neg!=0 || v.abs!=0 || v.neg_hi!=0 || v.opsel!=0
                || v.opsel_hi!=0 || v.clamp || v.omod!=0 || v.sdwa.is_some() || v.dpp.is_some() {
                return Err(err());
            }
            let dst=vector(v.dst.ok_or_else(err)?).ok_or_else(err)?;
            let r=Reg::V(dst);
            if dst==0||!writes(i,&[r]){return Err(err());}
            let args:Vec<u32>=v.src.iter().map(|o|e.operand(*o)).collect::<Result<_,_>>()?;
            let val=match (name,args.as_slice()) {
                ("v_mov_b32",&[a])=>a,
                ("v_add_nc_u32",&[a,b])=>e.u(128,a,b),
                ("v_sub_nc_u32",&[a,b])=>e.u(130,a,b),
                ("v_and_b32",&[a,b])=>e.u(199,a,b),
                ("v_or_b32",&[a,b])=>e.u(197,a,b),
                ("v_xor_b32",&[a,b])=>e.u(198,a,b),
                ("v_lshlrev_b32"|"v_lshrrev_b32",&[a,b])=>{
                    let m=e.c(31);let n=e.u(199,a,m);e.u(if name=="v_lshlrev_b32"{196}else{194},b,n)
                }
                _=>return Err(err()),
            };e.put(r,val)
        }
        Fields::Buffer{data,addr,rsrc,soffset,offset,offen,idxen,format,lds,tfe,cache}=>{
            if i.machine.encoding!=Encoding::Mubuf || *offen || !*idxen || offset%4!=0
                || format.is_some() || *lds || *tfe || cache.slc || cache.dlc
                || *rsrc!=(Operand::Scalar{code:4,dwords:4}) || *soffset!=Operand::Const(128)
                || (4..=7).any(|r|i.read(Reg::S(r)).is_none()) {return Err(err());}
            let address=vector(*addr).ok_or_else(err)?;
            let Some(Operand::Vector{reg,dwords})=*data else{return Err(err());};
            let atomic=name.starts_with("buffer_atomic_");
            let cmp=name=="buffer_atomic_cmpswap";
            if reg==0 || reg>31 || dwords!=if cmp{2}else{1} || (cmp&&reg==31) {return Err(err());}
            let r=Reg::V(reg);
            let expected=if name=="buffer_load_dword" || (atomic&&cache.glc){vec![r]}else{vec![]};
            if !writes(i,&expected){return Err(err());}
            if !atomic&&cache.glc{return Err(err());}
            // Nonatomic stores are private. Shared-address writes use atomics.
            if name=="buffer_store_dword" && (address!=0||*offset!=0){return Err(err());}
            let raw=e.reg(Reg::V(address))?;let off=e.c(u32::from(*offset)/4);
            let index=e.u(128,raw,off);let no_wrap=e.test(174,index,raw);
            let in_bounds=e.test(176,index,e.length);
            let safe=e.value(167,BOOL,&[no_wrap,in_bounds]);
            // Out-of-range load/returning atomic produces zero. Store is skipped.
            let old=if name=="buffer_load_dword"||atomic {Some(e.reg(r)?)}else{None};
            let compare=if cmp{Some(e.reg(Reg::V(reg+1))?)}else{None};
            if !expected.is_empty(){let z=e.c(0);e.put(r,z)?;}
            let merge=e.guard(safe);let z=e.c(0);
            let ptr=e.pointer(STORAGE_PTR,STORAGE,&[z,index]);
            match name {
                "buffer_load_dword"=>{let val=e.load(ptr);e.put(r,val)?;},
                "buffer_store_dword"=>{let val=e.reg(r)?;e.op(62,&[ptr,val]);},
                "buffer_atomic_add"|"buffer_atomic_umin"|"buffer_atomic_umax"|"buffer_atomic_swap"|"buffer_atomic_cmpswap"=>{
                    let scope=e.c(1);let sem=e.c(0);let val=old.ok_or_else(err)?;
                    let opcode=match name {"buffer_atomic_add"=>234,"buffer_atomic_umin"=>237,
                        "buffer_atomic_umax"=>239,"buffer_atomic_swap"=>229,_=>230};
                    let result=if cmp{e.value(opcode,UINT,&[ptr,scope,sem,sem,val,compare.ok_or_else(err)?])}
                        else{e.value(opcode,UINT,&[ptr,scope,sem,val])};
                    if cache.glc{e.put(r,result)?;}
                }
                _=>return Err(err()),
            }e.end_guard(merge);Ok(())
        }
        Fields::Ds{dst,addr,data,offset0,offset1,gds}=>{
            if *gds || *offset0!=0 || *offset1!=0{return Err(err());}
            let a=vector(addr.ok_or_else(err)?).ok_or_else(err)?;
            let address=e.reg(Reg::V(a))?;
            let three=e.c(3);let mask=e.u(199,address,three);let z=e.c(0);
            let aligned=e.test(170,mask,z);let two=e.c(2);let index=e.u(194,address,two);
            let limit=e.c(64);let in_bounds=e.test(176,index,limit);
            let safe=e.value(167,BOOL,&[aligned,in_bounds]);
            match name {
                "ds_write_b32"=>{
                    let &[source]=data.as_slice()else{return Err(err());};
                    if dst.is_some()||!writes(i,&[]){return Err(err());}
                    let val=e.operand(source)?;let merge=e.guard(safe);
                    let ptr=e.pointer(LDS_PTR,LDS,&[index]);e.op(62,&[ptr,val]);e.end_guard(merge);
                }
                "ds_read_b32"=>{
                    if !data.is_empty(){return Err(err());}
                    let r=Reg::V(vector(dst.ok_or_else(err)?).ok_or_else(err)?);
                    if !writes(i,&[r]){return Err(err());}
                    let z=e.c(0);e.put(r,z)?;let merge=e.guard(safe);
                    let ptr=e.pointer(LDS_PTR,LDS,&[index]);let val=e.load(ptr);e.put(r,val)?;e.end_guard(merge);
                }
                _=>return Err(err()),
            }Ok(())
        }
        _=>Err(err()),
    }
}

/// Compiles trusted native code with the explicit ABI above. All scalar inputs
/// are workgroup-uniform, so every lane visits the same block and barrier order.
pub fn emit_cfg(p:&ir::Program)->Result<Vec<u32>,ShaderError> {
    if p.blocks.is_empty()||p.blocks.len()>256||p.vgprs>32
        || p.blocks.iter().map(|b|b.insts.len()).sum::<usize>()>4096 {
        return Err(unsupported(0,"CFG size"));
    }
    if p.inputs.iter().any(|(r,_)|!matches!(r,Reg::S(0..=1)|Reg::V(0..=31))&&!descriptor(*r)&&!exec(*r)){
        return Err(unsupported(0,"undeclared CFG input"));
    }
    let mut e=Emit {next:32,globals:vec![],body:vec![],constants:BTreeMap::new(),
        regs:BTreeMap::new(),pc:0,budget:0,global_index:0,length:0};
    let mut used=std::collections::BTreeSet::new();
    for &(r,_) in &p.inputs {if ordinary(r){used.insert(r);}}
    for b in &p.blocks {
        for i in &b.insts {
            for &(r,_) in i.reads.iter().chain(i.writes.iter()) {if ordinary(r){used.insert(r);}}
        }
        for phi in &b.phis {if ordinary(phi.reg){used.insert(phi.reg);}else{return Err(unsupported(b.offset,"CFG phi register"));}}
        if b.exit==Exit::Indirect||b.exit.successors().iter().any(|&b|b>=p.blocks.len()){
            return Err(unsupported(b.offset,"CFG indirect or invalid target"));
        }
    }
    let entry=e.id();e.op(54,&[1,MAIN,0,2]);e.op(248,&[entry]);
    for r in used {let id=e.id();e.regs.insert(r,id);e.op(59,&[FUNCTION_PTR,id,7]);}
    e.pc=e.id();e.op(59,&[FUNCTION_PTR,e.pc,7]);
    e.budget=e.id();e.op(59,&[FUNCTION_PTR,e.budget,7]);
    let global=e.value(61,5,&[GLOBAL]);
    e.global_index=e.value(81,UINT,&[global,0]);
    e.length=e.value(68,UINT,&[STORAGE,0]);
    let regs:Vec<_>=e.regs.keys().copied().collect();
    for r in regs {
        let val=match r {
            Reg::S(n @ 0..=1)=>{let index=e.c(u32::from(n));let ptr=e.pointer(PUSH_PTR,PUSH,&[index]);e.load(ptr)},
            Reg::V(0)=>e.global_index,
            Reg::V(1)=>{let mask=e.c(63);e.u(199,e.global_index,mask)},
            Reg::V(2)=>e.load(SUBGROUP),
            _=>e.c(0),
        };e.put(r,val)?;
    }
    let zero=e.c(0);let one=e.c(1);let stop=e.c(u32::MAX);let cap=e.c(4096);
    e.op(62,&[e.pc,zero]);e.op(62,&[e.budget,cap]);
    let header=e.id();let dispatch=e.id();let cont=e.id();let end=e.id();let join=e.id();let default=e.id();
    let labels:Vec<u32>=(0..p.blocks.len()).map(|_|e.id()).collect();
    e.op(249,&[header]);e.op(248,&[header]);
    let pc=e.load(e.pc);let budget=e.load(e.budget);
    let live=e.test(171,pc,stop);let remaining=e.test(171,budget,zero);
    let cond=e.value(167,BOOL,&[live,remaining]);
    e.op(246,&[end,cont,0]);e.op(250,&[cond,dispatch,end]);
    e.op(248,&[dispatch]);e.op(247,&[join,0]);
    let mut cases=vec![pc,default];
    for (i,&label) in labels.iter().enumerate(){cases.extend([i as u32,label]);}
    e.op(251,&cases);
    for (b,&label) in p.blocks.iter().zip(&labels) {
        e.op(248,&[label]);
        for i in &b.insts {lower(&mut e,i)?;}
        let next=match b.exit {
            Exit::End=>stop,
            Exit::Jump(n)=>e.c(n as u32),
            Exit::Branch{taken,not_taken}=>{
                let last=b.insts.last().ok_or(unsupported(b.offset,"empty branch"))?;
                let opcode=match last.name(){"s_cbranch_scc0"=>170,"s_cbranch_scc1"=>171,
                    _=>return Err(unsupported(b.offset,"nonuniform or unsupported branch"))};
                let scc=e.reg(Reg::Scc)?;let cond=e.test(opcode,scc,zero);
                let yes=e.c(taken as u32);let no=e.c(not_taken as u32);e.select(cond,yes,no)
            }
            Exit::Indirect=>return Err(unsupported(b.offset,"indirect branch")),
        };
        e.op(62,&[e.pc,next]);e.op(249,&[join]);
    }
    e.op(248,&[default]);e.op(62,&[e.pc,stop]);e.op(249,&[join]);
    e.op(248,&[join]);e.op(249,&[cont]);e.op(248,&[cont]);
    let budget=e.load(e.budget);let next=e.u(130,budget,one);e.op(62,&[e.budget,next]);e.op(249,&[header]);
    e.op(248,&[end]);e.op(253,&[]);e.op(56,&[]);
    // Module layout: declarations, constants, globals, then function.
    let mut out=vec![0x07230203,0x00010300,0,e.next,0];
    let o=|out:&mut Vec<u32>,op,a:&[u32]|inst(out,op,a);
    o(&mut out,17,&[1]);o(&mut out,17,&[61]); // Shader, GroupNonUniform
    o(&mut out,14,&[0,1]);o(&mut out,15,&[5,MAIN,0x6e69616d,0,GLOBAL,SUBGROUP]);
    o(&mut out,16,&[MAIN,17,64,1,1]);
    o(&mut out,71,&[GLOBAL,11,28]);o(&mut out,71,&[SUBGROUP,11,36]);
    o(&mut out,71,&[6,6,4]);o(&mut out,71,&[7,2]);o(&mut out,72,&[7,0,35,0]);
    o(&mut out,71,&[STORAGE,34,0]);o(&mut out,71,&[STORAGE,33,0]);
    o(&mut out,71,&[8,2]);o(&mut out,72,&[8,0,35,0]);o(&mut out,72,&[8,1,35,4]);
    o(&mut out,19,&[1]);o(&mut out,33,&[2,1]);o(&mut out,21,&[UINT,32,0]);o(&mut out,20,&[BOOL]);
    o(&mut out,23,&[5,UINT,3]);o(&mut out,29,&[6,UINT]);o(&mut out,30,&[7,6]);
    o(&mut out,30,&[8,UINT,UINT]);o(&mut out,43,&[UINT,20,64]);o(&mut out,28,&[9,UINT,20]);
    for (id,storage,ty) in [(FUNCTION_PTR,7,UINT),(STORAGE_PTR,12,UINT),(PUSH_PTR,9,UINT),
        (LDS_PTR,4,UINT),(21,1,5),(22,12,7),(23,9,8),(24,4,9),(25,1,UINT)] {
        o(&mut out,32,&[id,storage,ty]);
    }
    out.extend(e.globals);
    for (ty,id,storage) in [(21,GLOBAL,1),(22,STORAGE,12),(23,PUSH,9),(24,LDS,4),(25,SUBGROUP,1)] {
        o(&mut out,59,&[ty,id,storage]);
    }
    out.extend(e.body);Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_loop_and_branch_emit() {
        let code:Vec<u8>=[0xbe82_0380u32,0xbe83_0380,0xbf09_0003,0xbf85_0003,
            0x8002_0102,0x8003_8103,0xbf82_fffb,0x7e06_0202,
            0xe070_2000,0x8001_0300,0xbf81_0000].into_iter().flat_map(u32::to_le_bytes).collect();
        let p=ir::build(&code,false,4).unwrap();
        assert!(p.blocks.len()>1);
        assert_eq!(emit_cfg(&p).unwrap()[0],0x07230203);
    }
}

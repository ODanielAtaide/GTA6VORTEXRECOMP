// SPDX-License-Identifier: GPL-3.0-or-later
//! Experimental native compute CFG backend. Unlike the straight-line diagnostic,
//! this lowers actual decoded block edges, scalar branches and loops. Register
//! state persists across edges, so phi joins are represented by mutable slots.
//! ABI: v0 initially holds the global index, v1 = local index, v2 = physical subgroup
//! size; other VGPRs initially zero; s0/s1 = push constants; s[4:7] = storage
//! descriptor 0:0 with stride four. EXEC starts full; immediate writes and
//! unsigned CMPX update per-lane state. EXECZ/EXECNZ are real subgroup votes.
//! Native memory effects only; no implicit output. One-dimensional workgroups of
//! 64. Dynamic dword indices are bounds checked. Ordinary stores may only use
//! the proven original global index and zero offset. Atomic accesses may share addresses. The caller must ensure
//! ordinary reads and LDS accesses are race free. LDS/barriers are rejected in
//! programs with EXEC mutations or wave-dependent branches. No indirect jumps,
//! arbitrary descriptors, VCC branches or general mask dataflow yet.
//! Multi-block execution is capped at 4096 basic-block visits to avoid unbounded GPU hangs.
//! FP32 arithmetic assumes ordinary finite values; guest FP MODE is not modeled.
use std::collections::BTreeMap;
use crate::{ShaderError, gfx10::{Fields, Operand, Encoding}, ir::{self, Reg, Exit, EXEC, VCC}};

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
const SUBGROUP_LANE:u32=26;
const MASK_TYPE:u32=27;
const STORAGE_SECOND:u32=28;
const FLOAT:u32=29;

fn inst(out:&mut Vec<u32>,opcode:u16,args:&[u32]) {
    out.push(((args.len() as u32+1)<<16)|u32::from(opcode));
    out.extend_from_slice(args);
}
// RDNA2 inline constant encodings, also documented by RDNA2 Commons
// engine/tools/rdna2_decode.py (blob d5037846572aea6a7864819c9d9e449019fbb8b1).
// Keep words in their native bit representation until the typed lowering.
fn inline_word(code: u8) -> Option<u32> {
    match code {
        128..=192 => Some(u32::from(code - 128)),
        193..=208 => Some(0u32.wrapping_sub(u32::from(code - 192))),
        240..=248 => {
            const FLOATS: [f32; 9] = [0.5, -0.5, 1.0, -1.0, 2.0, -2.0, 4.0, -4.0, 0.15915494];
            Some(FLOATS[usize::from(code - 240)].to_bits())
        }
        _ => None,
    }
}
fn unsupported(offset:usize,what:&'static str)->ShaderError {
    ShaderError::Unsupported { offset,what }
}
struct Emit {
    next:u32, globals:Vec<u32>, body:Vec<u32>, constants:BTreeMap<u32,u32>,
    regs:BTreeMap<Reg,u32>, pc:u32,budget:u32, active:u32, global_index:u32, length:u32, length_second:u32, wave64:bool, multi:bool, full_exec:bool,
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
    // Diagnostic ordinary FP32 semantics, not guest MODE/denormal emulation.
    fn fp(&mut self, code: u16, a: u32, b: u32) -> u32 {
        let a = self.value(124, FLOAT, &[a]);
        let b = self.value(124, FLOAT, &[b]);
        let f = self.value(code, FLOAT, &[a, b]);
        self.value(124, UINT, &[f])
    }
    // Signed addition/subtraction set SCC on overflow, not unsigned carry.
    fn signed_arithmetic(&mut self,a:u32,b:u32,sub:bool)->(u32,u32){
        let v=self.u(if sub{130}else{128},a,b);
        let first=self.u(198,a,if sub{b}else{v});
        let second=self.u(198,if sub{a}else{b},v);
        let changed=self.u(199,first,second);
        let sign=self.c(0x80000000);let overflow=self.u(199,changed,sign);
        let zero=self.c(0);let condition=self.test(171,overflow,zero);
        (v,self.flag(condition))
    }
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
            Operand::Const(n)=>inline_word(n).map(|word| self.c(word))
                .ok_or(unsupported(0,"CFG inline constant")),
            Operand::Scalar{code,dwords:1} if scalar(code)&&!(self.multi&&second_descriptor(Reg::S(code)))=>self.reg(Reg::S(code)),
            Operand::Vector{reg,dwords:1}=>self.reg(Reg::V(reg)),
            _=>Err(unsupported(0,"CFG operand")),
        }
    }
    fn active_condition(&mut self)->u32 {
        let a=self.load(self.active);let z=self.c(0);self.test(171,a,z)
    }
    fn ballot(&mut self,cond:u32)->[u32;2] {
        let scope=self.c(3);
        let v=self.value(339,MASK_TYPE,&[scope,cond]);
        [self.value(81,UINT,&[v,0]),self.value(81,UINT,&[v,1])]
    }
    fn exec_words(&mut self)->[u32;2] {
        let active=self.active_condition();self.ballot(active)
    }
    fn set_exec_words(&mut self,words:[u32;2]) {
        let lane=self.load(SUBGROUP_LANE);let thirtyone=self.c(31);let bit=self.u(199,lane,thirtyone);
        let word=if self.wave64 {
            let limit=self.c(32);let hi=self.test(174,lane,limit);self.select(hi,words[1],words[0])
        }else{words[0]};
        let shifted=self.u(194,word,bit);let one=self.c(1);let value=self.u(199,shifted,one);
        self.op(62,&[self.active,value]);
    }
    fn mask_operand(&mut self,o:Operand,width:u8)->Result<[u32;2],ShaderError> {
        match o {
            Operand::Scalar{code,dwords} if dwords==width=>{
                if code==EXEC {return Ok(self.exec_words());}
                if (0..width).any(|j|descriptor(Reg::S(code.saturating_add(j)))||(self.multi&&second_descriptor(Reg::S(code.saturating_add(j))))) {
                    return Err(unsupported(0,"resource descriptor used as mask"));
                }
                let lo=self.reg(Reg::S(code))?;
                let hi=if width==2{self.reg(Reg::S(code.checked_add(1).ok_or(unsupported(0,"mask range"))?))?}else{self.c(0)};
                Ok([lo,hi])
            }
            Operand::Const(193) => {let lo=self.c(u32::MAX);let hi=if width==2{lo}else{self.c(0)};Ok([lo,hi])}
            Operand::Const(128) => {let z=self.c(0);Ok([z,z])}
            _=>Err(unsupported(0,"unsupported mask operand")),
        }
    }
    fn put_mask(&mut self,code:u8,width:u8,words:[u32;2])->Result<(),ShaderError> {
        if code==EXEC {self.set_exec_words(words);return Ok(());}
        self.put(Reg::S(code),words[0])?;
        if width==2{self.put(Reg::S(code+1),words[1])?;}
        Ok(())
    }
    fn put_vector(&mut self,r:Reg,v:u32)->Result<(),ShaderError> {
        // Full EXEC is proven once per program, not guessed from an instruction.
        if self.full_exec { return self.put(r,v); }
        let old=self.reg(r)?;let cond=self.active_condition();let result=self.select(cond,v,old);
        self.put(r,result)
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
fn scalar(r:u8)->bool { (r<=105 && !(4..=7).contains(&r)) || r==VCC || r==VCC+1 }
fn ordinary(r:Reg)->bool { matches!(r,Reg::S(n) if scalar(n)) || matches!(r,Reg::Scc|Reg::V(_)) }
fn descriptor(r:Reg)->bool { matches!(r,Reg::S(4..=7)) }
fn second_descriptor(r:Reg)->bool { matches!(r,Reg::S(8..=11)) }
fn exec(r:Reg)->bool { matches!(r,Reg::S(n) if n==EXEC||n==EXEC+1) }
fn writes(inst:&ir::Inst,expected:&[Reg])->bool {
    inst.writes.len()==expected.len() && expected.iter().all(|r|inst.write(*r).is_some())
}
fn vector(o:Operand)->Option<u8> { match o { Operand::Vector{reg,dwords:1}=>Some(reg),_=>None } }

fn lower(e:&mut Emit,i:&ir::Inst)->Result<(),ShaderError> {
    let err=||unsupported(i.machine.offset,"native CFG instruction or operand");
    let name=i.name();
    if i.reads.iter().any(|(r,_)|!ordinary(*r)&&!descriptor(*r)&&!exec(*r))
        || i.writes.iter().any(|(r,_)|(!ordinary(*r)&&!exec(*r))||(e.multi&&second_descriptor(*r))) { return Err(err()); }
    match &i.fields {
        Fields::Sopp{..} => match name {
            "s_branch"|"s_cbranch_scc0"|"s_cbranch_scc1"|"s_cbranch_execz"|"s_cbranch_execnz"|"s_cbranch_vccz"|"s_cbranch_vccnz"|"s_endpgm" if i.writes.is_empty()=>Ok(()),
            "s_barrier" if i.writes.is_empty() => {
                // The preflight rejects wave-dependent control in barrier programs.
                let scope=e.c(2);let semantics=e.c(0x108);
                e.op(224,&[scope,scope,semantics]); Ok(())
            }
            _=>Err(err()),
        },
        Fields::Sop{dst,src,imm:Some(imm)} if i.machine.encoding==Encoding::Sopk=>{
            let Some(Operand::Scalar{code,dwords:1})=*dst else{return Err(err());};
            if !scalar(code)||!src.is_empty(){return Err(err());}
            let dest=Reg::S(code);
            let k=e.c(i32::from(*imm as i16) as u32);
            match name{
                "s_movk_i32" if writes(i,&[dest])=>e.put(dest,k),
                "s_mulk_i32" if writes(i,&[dest])=>{
                    let a=e.reg(dest)?;let v=e.u(132,a,k);e.put(dest,v)
                }
                "s_addk_i32" if writes(i,&[dest,Reg::Scc])=>{
                    let a=e.reg(dest)?;let(v,flag)=e.signed_arithmetic(a,k,false);
                    e.put(dest,v)?;e.put(Reg::Scc,flag)
                }
                _=>Err(err()),
            }
        },
        Fields::Sop{dst,src,imm:None}=>{
            let width=if e.wave64{2}else{1};
            let save=name.ends_with("saveexec_b32")||name.ends_with("saveexec_b64");
            let mask_move=matches!(name,"s_mov_b32"|"s_mov_b64") && matches!(dst,Some(Operand::Scalar{code,..}) if *code==EXEC)
                && matches!(src.as_slice(),[Operand::Scalar{..}]);
            if save||mask_move {
                let Some(Operand::Scalar{code,dwords})=*dst else{return Err(err());};
                if dwords!=width || (save && (exec(Reg::S(code))||code>104||!scalar(code)
                    || (width==2&&!scalar(code+1)))) {return Err(err());}
                let &[source]=src.as_slice()else{return Err(err());};
                let words=e.mask_operand(source,width)?;
                let mut expected:Vec<Reg>=(0..width).map(|j|Reg::S(code+j)).collect();
                if save {
                    let old=e.exec_words();
                    expected.extend((0..width).map(|j|Reg::S(EXEC+j)));expected.push(Reg::Scc);
                    if !writes(i,&expected){return Err(err());}
                    let mut next=[e.c(0);2];
                    for j in 0..usize::from(width) {
                        next[j]=match name {
                            "s_and_saveexec_b32"|"s_and_saveexec_b64"=>e.u(199,old[j],words[j]),
                            "s_or_saveexec_b32"|"s_or_saveexec_b64"=>e.u(197,old[j],words[j]),
                            "s_xor_saveexec_b32"|"s_xor_saveexec_b64"=>e.u(198,old[j],words[j]),
                            "s_andn2_saveexec_b32"|"s_andn2_saveexec_b64"=>{
                                let n=e.value(200,UINT,&[words[j]]);e.u(199,old[j],n)
                            }
                            _=>return Err(err()),
                        };
                    }
                    e.put_mask(code,width,old)?;e.set_exec_words(next);
                    let union=e.u(197,next[0],next[1]);let z=e.c(0);let cond=e.test(171,union,z);
                    let flag=e.flag(cond);e.put(Reg::Scc,flag)?;
                }else{
                    if !writes(i,&expected){return Err(err());}
                    e.set_exec_words(words);
                }
                return Ok(());
            }
            if let Some(Operand::Scalar{code,dwords:1})=*dst {
                if exec(Reg::S(code)) {
                    if name!="s_mov_b32" || !i.reads.is_empty() || !writes(i,&[Reg::S(code)])
                        || (code==EXEC+1&&!e.wave64) {return Err(err());}
                    let &[source]=src.as_slice() else{return Err(err());};
                    if !matches!(source,Operand::Literal(_)|Operand::Const(128..=208)) {return Err(err());}
                    let value=e.operand(source)?;
                    let lane=e.load(SUBGROUP_LANE);let mask=e.c(31);let bit=e.u(199,lane,mask);
                    let shifted=e.u(194,value,bit);let one=e.c(1);let new=e.u(199,shifted,one);
                    let result=if e.wave64 {
                        let limit=e.c(32);let high=e.test(174,lane,limit);
                        let old=e.load(e.active);
                        if code==EXEC+1 {e.select(high,new,old)}else{e.select(high,old,new)}
                    }else{new};
                    e.op(62,&[e.active,result]);return Ok(());
                }
            }
            if src.iter().any(|o|matches!(o,Operand::Vector{..})) { return Err(err()); }
            let args:Vec<u32>=src.iter().map(|o|e.operand(*o)).collect::<Result<_,_>>()?;
            if dst.is_none() {
                let &[a,b]=args.as_slice() else {return Err(err());};
                let opcode=match name {
                    "s_cmp_eq_u32"|"s_cmp_eq_i32"=>170,"s_cmp_lg_u32"|"s_cmp_lg_i32"=>171,
                    "s_cmp_gt_u32"=>172,"s_cmp_ge_u32"=>174,
                    "s_cmp_lt_u32"=>176,"s_cmp_le_u32"=>178,
                    "s_cmp_gt_i32"=>173,"s_cmp_ge_i32"=>175,
                    "s_cmp_lt_i32"=>177,"s_cmp_le_i32"=>179,_=>return Err(err()),
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
                ("s_add_i32"|"s_sub_i32",&[a,b])=>{
                    let(v,f)=e.signed_arithmetic(a,b,name=="s_sub_i32");(v,Some(f))
                },
                ("s_addc_u32",&[a,b])=>{
                    let c=e.reg(Reg::Scc)?;let one=e.c(1);let c=e.u(199,c,one);
                    let sum=e.u(128,a,b);let v=e.u(128,sum,c);
                    let first=e.test(176,sum,a);let second=e.test(176,v,sum);
                    let overflow=e.value(166,BOOL,&[first,second]);(v,Some(e.flag(overflow)))
                },
                ("s_subb_u32",&[a,b])=>{
                    let c=e.reg(Reg::Scc)?;let one=e.c(1);let c=e.u(199,c,one);
                    let diff=e.u(130,a,b);let v=e.u(130,diff,c);
                    let first=e.test(176,a,b);let second=e.test(176,diff,c);
                    let borrow=e.value(166,BOOL,&[first,second]);(v,Some(e.flag(borrow)))
                },
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
            let mbcnt=matches!(name,"v_mbcnt_lo_u32_b32"|"v_mbcnt_hi_u32_b32");
            let integer_vop3=matches!(name,"v_lshl_add_u32"|"v_lshl_or_b32"|"v_add_lshl_u32"|"v_and_or_b32"|"v_or3_b32"|"v_add3_u32"|"v_xor3_b32"|"v_mul_lo_u32"|"v_min_u32"|"v_max_u32"|"v_bcnt_u32_b32"|"v_subrev_nc_u32");
            if !(matches!(i.machine.encoding,Encoding::Vop1|Encoding::Vop2|Encoding::Vopc)||mbcnt||integer_vop3||(i.machine.encoding==Encoding::Vop3&&name=="v_cndmask_b32"))
                || v.carry.is_some() || v.neg!=0 || v.abs!=0 || v.neg_hi!=0 || v.opsel!=0
                || v.opsel_hi!=0 || v.clamp || v.omod!=0 || v.sdwa.is_some() || v.dpp.is_some() {
                return Err(err());
            }
            if i.machine.encoding==Encoding::Vopc {
                let args:Vec<u32>=v.src.iter().map(|o|e.operand(*o)).collect::<Result<_,_>>()?;
                let &[a,b]=args.as_slice() else{return Err(err());};
                let opcode=match name {"v_cmpx_lt_u32"|"v_cmp_lt_u32"=>176,"v_cmpx_gt_u32"|"v_cmp_gt_u32"=>172,
                    "v_cmpx_le_u32"|"v_cmp_le_u32"=>178,"v_cmpx_ge_u32"|"v_cmp_ge_u32"=>174,"v_cmpx_eq_u32"|"v_cmp_eq_u32"=>170,
                    "v_cmpx_ne_u32"|"v_cmpx_lg_u32"|"v_cmp_ne_u32"=>171,_=>return Err(err())};
                let target=if name.starts_with("v_cmpx"){EXEC}else{VCC};
                let expected=if e.wave64{vec![Reg::S(target),Reg::S(target+1)]}else{vec![Reg::S(target)]};
                if !writes(i,&expected){return Err(err());}
                let cmp=e.test(opcode,a,b);let active=e.active_condition();
                let result=e.value(167,BOOL,&[cmp,active]);
                if target==EXEC {let bit=e.flag(result);e.op(62,&[e.active,bit]);}
                else {let mask=e.ballot(result);e.put_mask(VCC,if e.wave64{2}else{1},mask)?;}
                return Ok(());
            }
            let dst=vector(v.dst.ok_or_else(err)?).ok_or_else(err)?;
            let r=Reg::V(dst);
            if !writes(i,&[r]){return Err(err());}
            if name=="v_cndmask_b32" {
                let &[a,b,mask]=v.src.as_slice() else { return Err(err()); };
                let a=e.operand(a)?; let b=e.operand(b)?;
                let words=e.mask_operand(mask,if e.wave64{2}else{1})?;
                let lane=e.load(SUBGROUP_LANE);
                let low_bits=e.c(31); let bit=e.u(199,lane,low_bits);
                let word=if e.wave64 {
                    let split=e.c(32); let upper=e.test(174,lane,split);
                    e.select(upper,words[1],words[0])
                }else{words[0]};
                let shifted=e.u(194,word,bit); let one=e.c(1);
                let selected=e.u(199,shifted,one);
                let zero=e.c(0); let cond=e.test(171,selected,zero);
                let result=e.select(cond,b,a);
                return e.put_vector(r,result);
            }
            if mbcnt{
                let &[mask,base]=v.src.as_slice()else{return Err(err());};
                let a=match mask{
                    Operand::Scalar{code,dwords:1} if code==EXEC||code==EXEC+1=>{
                        let halves=e.exec_words();if code==EXEC{halves[0]}else if e.wave64{halves[1]}else{e.c(0)}
                    }
                    _=>e.operand(mask)?,
                };
                let b=e.operand(base)?;
                let lane=e.load(SUBGROUP_LANE);
                let one=e.c(1);let zero=e.c(0);let thirtytwo=e.c(32);let thirtyone=e.c(31);let ones=e.c(u32::MAX);
                let below=if name=="v_mbcnt_hi_u32_b32"{
                    let lower=e.test(176,lane,thirtytwo);let n=e.u(130,lane,thirtytwo);e.select(lower,zero,n)
                }else{lane};
                let shift=e.u(199,below,thirtyone);
                let bit=e.u(196,one,shift);let prefix=e.u(130,bit,one);
                let full=e.test(174,below,thirtytwo);let prefix=e.select(full,ones,prefix);
                let masked=e.u(199,a,prefix);let count=e.value(205,UINT,&[masked]);let result=e.u(128,b,count);
                return e.put_vector(r,result);
            }
            let args:Vec<u32>=v.src.iter().map(|o|e.operand(*o)).collect::<Result<_,_>>()?;
            let val=match (name,args.as_slice()) {
                ("v_mov_b32",&[a])=>a,
                ("v_add_f32",&[a,b])=>e.fp(129,a,b),
                ("v_sub_f32",&[a,b])=>e.fp(131,a,b),
                ("v_subrev_f32",&[a,b])=>e.fp(131,b,a),
                ("v_mul_f32",&[a,b])=>e.fp(133,a,b),
                ("v_cvt_f32_u32",&[a])=>{
                    let f=e.value(112,FLOAT,&[a]); e.value(124,UINT,&[f])
                }
                ("v_add_nc_u32",&[a,b])=>e.u(128,a,b),
                ("v_sub_nc_u32",&[a,b])=>e.u(130,a,b),
                ("v_and_b32",&[a,b])=>e.u(199,a,b),
                ("v_or_b32",&[a,b])=>e.u(197,a,b),
                ("v_xor_b32",&[a,b])=>e.u(198,a,b),
                ("v_subrev_nc_u32",&[a,b])=>e.u(130,b,a),
                ("v_mul_lo_u32",&[a,b])=>e.u(132,a,b),
                ("v_min_u32"|"v_max_u32",&[a,b])=>{
                    let cond=e.test(if name=="v_min_u32"{176}else{172},a,b);
                    e.select(cond,a,b)
                }
                ("v_bcnt_u32_b32",&[a,b])=>{
                    let count=e.value(205,UINT,&[a]);e.u(128,count,b)
                }
                ("v_lshl_add_u32"|"v_lshl_or_b32",&[a,b,c])=>{
                    let mask=e.c(31);let shift=e.u(199,b,mask);let t=e.u(196,a,shift);
                    e.u(if name=="v_lshl_add_u32"{128}else{197},t,c)
                }
                ("v_add_lshl_u32",&[a,b,c])=>{
                    let sum=e.u(128,a,b);let mask=e.c(31);let shift=e.u(199,c,mask);e.u(196,sum,shift)
                }
                ("v_and_or_b32"|"v_or3_b32"|"v_add3_u32"|"v_xor3_b32",&[a,b,c])=>{
                    let (first,second)=match name{"v_and_or_b32"=>(199,197),"v_or3_b32"=>(197,197),"v_add3_u32"=>(128,128),_=>(198,198)};
                    let t=e.u(first,a,b);e.u(second,t,c)
                }
                ("v_lshlrev_b32"|"v_lshrrev_b32",&[a,b])=>{
                    let m=e.c(31);let n=e.u(199,a,m);e.u(if name=="v_lshlrev_b32"{196}else{194},b,n)
                }
                _=>return Err(err()),
            };e.put_vector(r,val)
        }
        Fields::Buffer{data,addr,rsrc,soffset,offset,offen,idxen,format,lds,tfe,cache}=>{
            if i.machine.encoding!=Encoding::Mubuf || *offen || !*idxen || offset%4!=0
                || format.is_some() || *lds || *tfe || cache.slc || cache.dlc
                || *soffset!=Operand::Const(128) {return Err(err());}
            let (base,length,sgpr)=match *rsrc {
                Operand::Scalar{code:4,dwords:4}=>(STORAGE,e.length,4),
                Operand::Scalar{code:8,dwords:4} if e.multi=>(STORAGE_SECOND,e.length_second,8),
                _=>return Err(err()),
            };
            if (sgpr..sgpr+4).any(|r|i.read(Reg::S(r)).is_none()){return Err(err());}
            let address=vector(*addr).ok_or_else(err)?;
            let Some(Operand::Vector{reg,dwords})=*data else{return Err(err());};
            let atomic=name.starts_with("buffer_atomic_");
            let cmp=name=="buffer_atomic_cmpswap";
            if dwords!=(if cmp{2}else{1}) || (cmp&&reg==u8::MAX) {return Err(err());}
            let r=Reg::V(reg);
            let expected=if name=="buffer_load_dword" || (atomic&&cache.glc){vec![r]}else{vec![]};
            if !writes(i,&expected){return Err(err());}
            if !atomic&&cache.glc{return Err(err());}
            // Nonatomic stores are private. Shared-address writes use atomics.
            // The module preflight proves the index is invocation-private.
            if name=="buffer_store_dword" && *offset!=0{return Err(err());}
            let raw=e.reg(Reg::V(address))?;let off=e.c(u32::from(*offset)/4);
            let index=e.u(128,raw,off);let no_wrap=e.test(174,index,raw);
            let in_bounds=e.test(176,index,length);
            let bounded=e.value(167,BOOL,&[no_wrap,in_bounds]);
            let active=e.active_condition();let safe=e.value(167,BOOL,&[bounded,active]);
            // Out-of-range load/returning atomic produces zero. Store is skipped.
            let old=if name=="buffer_load_dword"||atomic {Some(e.reg(r)?)}else{None};
            let compare=if cmp{Some(e.reg(Reg::V(reg+1))?)}else{None};
            if !expected.is_empty(){let z=e.c(0);e.put_vector(r,z)?;}
            let merge=e.guard(safe);let z=e.c(0);
            let ptr=e.pointer(STORAGE_PTR,base,&[z,index]);
            match name {
                "buffer_load_dword"=>{let val=e.load(ptr);e.put(r,val)?;},
                "buffer_store_dword"=>{let val=e.reg(r)?;e.op(62,&[ptr,val]);},
                "buffer_atomic_add"|"buffer_atomic_sub"|"buffer_atomic_and"|"buffer_atomic_or"|"buffer_atomic_xor"|
                "buffer_atomic_umin"|"buffer_atomic_umax"|"buffer_atomic_swap"|"buffer_atomic_cmpswap"=>{
                    let scope=e.c(1);let sem=e.c(0);let val=old.ok_or_else(err)?;
                    let opcode=match name {
                        "buffer_atomic_add"=>234,"buffer_atomic_sub"=>235,
                        "buffer_atomic_and"=>240,"buffer_atomic_or"=>241,"buffer_atomic_xor"=>242,
                        "buffer_atomic_umin"=>237,"buffer_atomic_umax"=>239,
                        "buffer_atomic_swap"=>229,"buffer_atomic_cmpswap"=>230,_=>return Err(err()),
                    };
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

/// Instruction-local lowering diagnostics only. Every instruction is probed
/// independently with symbolic register slots; no module is built or executed.
/// These counts deliberately do NOT certify resource mappings, CFG, phi,
/// subgroup convergence, or cross-invocation memory safety.
pub fn diagnose_instructions(p:&ir::Program)->BTreeMap<String,u64>{
    let mut gaps=BTreeMap::new();
    let mut regs=BTreeMap::new();
    let mut next=64u32;
    for r in (0..=u8::MAX).map(Reg::V).chain((0..=127u8).map(Reg::S)).chain([Reg::Scc]){
        if ordinary(r){regs.insert(r,next);next+=1;}
    }
    let mut e=Emit{next,globals:vec![],body:vec![],constants:BTreeMap::new(),regs,
        pc:31,budget:32,active:33,global_index:34,length:35,length_second:36,wave64:p.wave64,multi:false,full_exec:false};
    for i in p.blocks.iter().flat_map(|b|&b.insts){
        e.body.clear();e.globals.clear();e.constants.clear();e.next=next;
        if let Err(error)=lower(&mut e,i){
            let reason=match error{ShaderError::Unsupported{what,..}=>what,ShaderError::Isa{reason,..}=>reason,_=>"other"};
            *gaps.entry(format!("{}: {}",i.name(),reason)).or_default()+=1;
        }
    }
    gaps
}

/// Compiles trusted native code with the explicit ABI above. Scalar flow is
/// uniform within a physical subgroup; wave branches vote across all its lanes.
fn has_wave_flow(p:&ir::Program)->bool {
    p.blocks.iter().flat_map(|b|&b.insts).any(|i|
        i.writes.iter().any(|(r,_)|exec(*r)||matches!(r,Reg::S(106..=107))) || matches!(i.name(),"s_cbranch_execz"|"s_cbranch_execnz"|"s_cbranch_vccz"|"s_cbranch_vccnz"))
}

/// Barrier participation is workgroup-uniform only for the scalar-only path.
pub fn emit_cfg(p:&ir::Program)->Result<Vec<u32>,ShaderError> { emit_cfg_inner(p,false) }

/// Explicit two-resource ABI: s[4:7] -> binding 0, s[8:11] -> binding 1.
pub fn emit_cfg_two_resources(p:&ir::Program)->Result<Vec<u32>,ShaderError> { emit_cfg_inner(p,true) }

// Prove identity to the original global index in SSA, rather than requiring
// a particular physical register. Start with the conservative full-EXEC,
// straight-line case. Partial EXEC preserves old lane values, so it must not
// propagate a copy unless both masked alternatives are known (not modeled yet).
fn check_private_stores(p: &ir::Program) -> Result<(), ShaderError> {
    let mut private = std::collections::BTreeSet::new();
    if let Some((_, v)) = p.inputs.iter().find(|(r, _)| *r == Reg::V(0)) {
        private.insert(*v);
    }
    let copies = p.blocks.len() == 1 && !has_wave_flow(p);
    for i in p.blocks.iter().flat_map(|b| &b.insts) {
        if i.name() == "buffer_store_dword" {
            let address = match &i.fields {
                Fields::Buffer { addr, offset: 0, .. } => vector(*addr),
                _ => None,
            };
            if !address.and_then(|reg| i.read(Reg::V(reg))).is_some_and(|v| private.contains(&v)) {
                return Err(unsupported(i.machine.offset, "CFG store requires unchanged global index"));
            }
        }
        if copies && i.name() == "v_mov_b32" && i.machine.encoding == Encoding::Vop1 {
            if let Fields::Valu(v) = &i.fields {
                if v.dpp.is_none() && v.sdwa.is_none() {
                    if let [source] = v.src.as_slice() {
                        if let (Some(src), Some(dst)) = (vector(*source), v.dst.and_then(vector)) {
                            if i.read(Reg::V(src)).is_some_and(|id| private.contains(&id)) {
                                if let Some(id) = i.write(Reg::V(dst)) { private.insert(id); }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn emit_cfg_inner(p:&ir::Program,multi:bool)->Result<Vec<u32>,ShaderError> {
    if p.blocks.is_empty()||p.blocks.len()>1024 {
        return Err(unsupported(0,"CFG block count"));
    }
    if p.vgprs>256 {return Err(unsupported(0,"CFG VGPR count"));}
    if p.blocks.iter().map(|b|b.insts.len()).sum::<usize>()>16384{
        return Err(unsupported(0,"CFG instruction count"));
    }
    if p.inputs.iter().any(|(r,_)|!matches!(r,Reg::S(0..=1)|Reg::V(_))&&!descriptor(*r)&&!(multi&&second_descriptor(*r))&&!exec(*r)){
        return Err(unsupported(0,"undeclared CFG input"));
    }
    check_private_stores(p)?;
    if has_wave_flow(p)&&p.blocks.iter().flat_map(|b|&b.insts).any(|i|
        i.name()=="s_barrier"||matches!(i.fields,Fields::Ds{..})) {
        return Err(unsupported(0,"LDS or barrier in wave-dependent CFG"));
    }
    let mut e=Emit {next:32,globals:vec![],body:vec![],constants:BTreeMap::new(),
        regs:BTreeMap::new(),pc:0,budget:0,active:0,global_index:0,length:0,length_second:0,wave64:p.wave64,multi,full_exec:!has_wave_flow(p)};
    let mut used=std::collections::BTreeSet::new();
    for &(r,_) in &p.inputs {if ordinary(r)&&!(multi&&second_descriptor(r)){used.insert(r);}}
    for b in &p.blocks {
        for i in &b.insts {
            for &(r,_) in i.reads.iter().chain(i.writes.iter()) {if ordinary(r)&&!(multi&&second_descriptor(r)){used.insert(r);}}
        }
        for phi in &b.phis {if ordinary(phi.reg)&&!(multi&&second_descriptor(phi.reg)){used.insert(phi.reg);}else if !exec(phi.reg){return Err(unsupported(b.offset,"CFG phi register"));}}
        if b.exit==Exit::Indirect||b.exit.successors().iter().any(|&b|b>=p.blocks.len()){
            return Err(unsupported(b.offset,"CFG indirect or invalid target"));
        }
    }
    let entry=e.id();e.op(54,&[1,MAIN,0,2]);e.op(248,&[entry]);
    for r in used {let id=e.id();e.regs.insert(r,id);e.op(59,&[FUNCTION_PTR,id,7]);}
    let straight = p.blocks.len() == 1 && p.blocks[0].exit == Exit::End;
    if !straight {
        e.pc=e.id();e.op(59,&[FUNCTION_PTR,e.pc,7]);
        e.budget=e.id();e.op(59,&[FUNCTION_PTR,e.budget,7]);
    }
    e.active=e.id();e.op(59,&[FUNCTION_PTR,e.active,7]);
    let global=e.value(61,5,&[GLOBAL]);
    e.global_index=e.value(81,UINT,&[global,0]);
    e.length=e.value(68,UINT,&[STORAGE,0]);
    if multi { e.length_second=e.value(68,UINT,&[STORAGE_SECOND,0]); }
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
    let one=e.c(1);e.op(62,&[e.active,one]);
    if straight {
        // No synthetic program counter, switch, or visit budget is needed.
        // Instruction-local guards and EXEC semantics remain identical.
        for i in &p.blocks[0].insts { lower(&mut e,i)?; }
    } else {
        let zero=e.c(0);let stop=e.c(u32::MAX);let cap=e.c(4096);
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
                    let cond=match last.name() {
                        "s_cbranch_scc0"|"s_cbranch_scc1"=>{
                            let scc=e.reg(Reg::Scc)?;
                            e.test(if last.name()=="s_cbranch_scc0"{170}else{171},scc,zero)
                        }
                        "s_cbranch_vccz"|"s_cbranch_vccnz"=>{
                            let lo=e.reg(Reg::S(VCC))?;
                            let hi=if e.wave64{e.reg(Reg::S(VCC+1))?}else{zero};
                            let mask=e.u(197,lo,hi);
                            e.test(if last.name()=="s_cbranch_vccz"{170}else{171},mask,zero)
                        }
                        "s_cbranch_execz"|"s_cbranch_execnz"=>{
                            let active=e.active_condition();let scope=e.c(3);
                            let any=e.value(335,BOOL,&[scope,active]);
                            if last.name()=="s_cbranch_execz"{e.value(168,BOOL,&[any])}else{any}
                        }
                        _=>return Err(unsupported(b.offset,"unsupported branch condition")),
                    };
                    let yes=e.c(taken as u32);let no=e.c(not_taken as u32);e.select(cond,yes,no)
                }
                Exit::Indirect=>return Err(unsupported(b.offset,"indirect branch")),
            };
            e.op(62,&[e.pc,next]);e.op(249,&[join]);
        }
        e.op(248,&[default]);e.op(62,&[e.pc,stop]);e.op(249,&[join]);
        e.op(248,&[join]);e.op(249,&[cont]);e.op(248,&[cont]);
        let budget=e.load(e.budget);let next=e.u(130,budget,one);e.op(62,&[e.budget,next]);e.op(249,&[header]);
        e.op(248,&[end]);
    }
    e.op(253,&[]);e.op(56,&[]);
    // Module layout: declarations, constants, globals, then function.
    let mut out=vec![0x07230203,0x00010300,0,e.next,0];
    let o=|out:&mut Vec<u32>,op,a:&[u32]|inst(out,op,a);
    o(&mut out,17,&[1]);o(&mut out,17,&[61]);o(&mut out,17,&[62]);o(&mut out,17,&[64]); // Shader, GroupNonUniform, Vote, Ballot
    o(&mut out,14,&[0,1]);o(&mut out,15,&[5,MAIN,0x6e69616d,0,GLOBAL,SUBGROUP,SUBGROUP_LANE]);
    o(&mut out,16,&[MAIN,17,64,1,1]);
    o(&mut out,71,&[GLOBAL,11,28]);o(&mut out,71,&[SUBGROUP,11,36]);
    o(&mut out,71,&[SUBGROUP_LANE,11,41]);
    o(&mut out,71,&[6,6,4]);o(&mut out,71,&[7,2]);o(&mut out,72,&[7,0,35,0]);
    o(&mut out,71,&[STORAGE,34,0]);o(&mut out,71,&[STORAGE,33,0]);
    if multi {o(&mut out,71,&[STORAGE_SECOND,34,0]);o(&mut out,71,&[STORAGE_SECOND,33,1]);}
    o(&mut out,71,&[8,2]);o(&mut out,72,&[8,0,35,0]);o(&mut out,72,&[8,1,35,4]);
    o(&mut out,19,&[1]);o(&mut out,33,&[2,1]);o(&mut out,21,&[UINT,32,0]);o(&mut out,20,&[BOOL]);
    o(&mut out,22,&[FLOAT,32]);
    o(&mut out,23,&[5,UINT,3]);o(&mut out,23,&[MASK_TYPE,UINT,4]);o(&mut out,29,&[6,UINT]);o(&mut out,30,&[7,6]);
    o(&mut out,30,&[8,UINT,UINT]);o(&mut out,43,&[UINT,20,64]);o(&mut out,28,&[9,UINT,20]);
    for (id,storage,ty) in [(FUNCTION_PTR,7,UINT),(STORAGE_PTR,12,UINT),(PUSH_PTR,9,UINT),
        (LDS_PTR,4,UINT),(21,1,5),(22,12,7),(23,9,8),(24,4,9),(25,1,UINT)] {
        o(&mut out,32,&[id,storage,ty]);
    }
    out.extend(e.globals);
    for (ty,id,storage) in [(21,GLOBAL,1),(22,STORAGE,12),(23,PUSH,9),(24,LDS,4),(25,SUBGROUP,1),(25,SUBGROUP_LANE,1)] {
        o(&mut out,59,&[ty,id,storage]);
    }
    if multi {o(&mut out,59,&[22,STORAGE_SECOND,12]);}
    out.extend(e.body);Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn has_opcode(words: &[u32], opcode: u32) -> bool {
        let mut pos = 5;
        while pos < words.len() {
            let count = (words[pos] >> 16) as usize;
            assert!(count > 0 && count <= words.len() - pos);
            if words[pos] & 0xffff == opcode { return true; }
            pos += count;
        }
        false
    }
    #[test]
    fn inline_constants_preserve_word_bits() {
        assert_eq!(inline_word(128), Some(0));
        assert_eq!(inline_word(193), Some(u32::MAX));
        assert_eq!(inline_word(241), Some((-0.5f32).to_bits()));
        assert_eq!(inline_word(242), Some(1.0f32.to_bits()));
        assert_eq!(inline_word(248), Some(0x3e22f983));
        assert_eq!(inline_word(249), None);
    }
    #[test]
    fn native_fp32_mul_emits_typed_operation() {
        // v_mov_b32 v7, 0.25; v_mul_f32 v7, 1.0, v7; s_endpgm.
        let code: Vec<u8> = [0x7e0e_02ffu32, 0.25f32.to_bits(), 0x1e0e_0ef2, 0xbf81_0000]
            .into_iter().flat_map(u32::to_le_bytes).collect();
        let p = ir::build(&code, false, 8).unwrap();
        let words = emit_cfg(&p).unwrap();
        assert!(has_opcode(&words, 133)); // OpFMul
        assert!(has_opcode(&words, 124)); // OpBitcast
    }
    #[test]
    fn straight_line_skips_dispatcher() {
        let code: Vec<u8> = [0x4a02_0200u32, 0x3a04_0201, 0xbf81_0000]
            .into_iter().flat_map(u32::to_le_bytes).collect();
        let p = ir::build(&code, false, 4).unwrap();
        assert_eq!(p.blocks.len(), 1);
        let words = emit_cfg(&p).unwrap();
        assert_eq!(words[0], 0x07230203);
        assert!(!has_opcode(&words, 246)); // OpLoopMerge
        assert!(!has_opcode(&words, 251)); // OpSwitch
        assert!(!has_opcode(&words, 169)); // No EXEC selects for full-EXEC arithmetic
    }
    #[test]
    fn cndmask_uses_native_wave_mask() {
        // v_cmp_eq_u32 vcc, 0, v0; v_cndmask_b32 v1, 0, v0, vcc.
        let code: Vec<u8> = [0x7d84_0080u32,0x0202_0080,0xbf81_0000]
            .into_iter().flat_map(u32::to_le_bytes).collect();
        for wave64 in [false,true] {
            let p = ir::build(&code, wave64, 4).unwrap();
            let words = emit_cfg(&p).unwrap();
            assert!(has_opcode(&words, 339)); // OpGroupNonUniformBallot
            assert!(has_opcode(&words, 169)); // OpSelect
        }
    }
    #[test]
    fn private_store_accepts_proven_copy_not_arbitrary_index() {
        // v_mov_b32 v2, v0; buffer_store_dword v3, v2, s[4:7], 0 idxen.
        let build = |mov| {
            let code: Vec<u8> = [mov, 0xe070_2000u32, 0x8001_0302, 0xbf81_0000]
                .into_iter().flat_map(u32::to_le_bytes).collect();
            ir::build(&code, false, 4).unwrap()
        };
        let copied = build(0x7e04_0300);
        assert!(check_private_stores(&copied).is_ok());
        assert!(emit_cfg(&copied).is_ok());
        let constant = build(0x7e04_0280);
        assert!(check_private_stores(&constant).is_err());
        assert!(emit_cfg(&constant).is_err());
    }
    #[test]
    fn scalar_signed_immediates_and_carry_emit(){
        // s_movk_i32 s2,-1; s_addk_i32 s2,1; s_add_i32 s3,s2,1;
        // s_addc_u32 s3,s3,0; s_cmp_lt_i32 s3,0; s_endpgm
        let code:Vec<u8>=[0xb002_ffffu32,0xb782_0001,0x8103_8102,0x8203_8003,
            0xbf04_8003,0xbf81_0000].into_iter().flat_map(u32::to_le_bytes).collect();
        let p=ir::build(&code,false,1).unwrap();
        assert!(diagnose_instructions(&p).is_empty(),"{:?}",diagnose_instructions(&p));
        let words=emit_cfg(&p).unwrap();
        assert!(has_opcode(&words,166));
        assert!(has_opcode(&words,177));
    }
    #[test]
    fn native_loop_and_branch_emit() {
        let code:Vec<u8>=[0xbe82_0380u32,0xbe83_0380,0xbf09_0003,0xbf85_0003,
            0x8002_0102,0x8003_8103,0xbf82_fffb,0x7e06_0202,
            0xe070_2000,0x8001_0300,0xbf81_0000].into_iter().flat_map(u32::to_le_bytes).collect();
        let p=ir::build(&code,false,4).unwrap();
        assert!(p.blocks.len()>1);
        let words = emit_cfg(&p).unwrap();
        assert_eq!(words[0],0x07230203);
        assert!(has_opcode(&words, 246));
        assert!(has_opcode(&words, 251));
        assert!(diagnose_instructions(&p).is_empty());
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Initial graphics stage lowering with an explicit test ABI.
//! Vertex v0..v2 = location 0 xyz, v3..v6 = location 1 rgba.
//! Pixel v0..v3 = interpolated location 0 rgba. This is NOT the console
//! fetch/interpolation ABI; no inference of native shader signatures yet.
//! Full EXEC, straight line, one POS0 + PARAM0 or MRT0 export, uncompressed.
//! Reject unimplemented effects rather than silently skipping them.
use crate::{ShaderError,gfx10::{Encoding,Fields,Operand},ir::{self,Reg,Value,EXEC}};
use std::collections::{BTreeMap,BTreeSet};

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Stage { Vertex, Fragment }

const VOID:u32=1;
const FN:u32=2;
const UINT:u32=3;
const FLOAT:u32=4;
const VEC3:u32=5;
const VEC4:u32=6;
const IN3:u32=7;
const IN4:u32=8;
const OUT4:u32=9;
const INPUT0:u32=10;
const INPUT1:u32=11;
const OUTPUT0:u32=12;
const POSITION:u32=13;
const MAIN:u32=14;
const ENTRY:u32=15;

fn op(out:&mut Vec<u32>,code:u16,a:&[u32]) {
    out.push(((a.len() as u32+1)<<16)|u32::from(code));out.extend_from_slice(a);
}
fn err(offset:usize,what:&'static str)->ShaderError {ShaderError::Unsupported{offset,what}}
struct Emit {
    next:u32, constants:BTreeMap<u32,u32>, declarations:Vec<u32>, body:Vec<u32>,
    values:BTreeMap<Value,u32>, defined:BTreeSet<Reg>,
}
impl Emit {
    fn id(&mut self)->u32{let i=self.next;self.next+=1;i}
    fn value(&mut self,opcode:u16,ty:u32,a:&[u32])->u32 {
        let id=self.id();let mut args=vec![ty,id];args.extend_from_slice(a);
        op(&mut self.body,opcode,&args);id
    }
    fn constant(&mut self,n:u32)->u32 {
        if let Some(&id)=self.constants.get(&n){return id;}
        let id=self.id();self.constants.insert(n,id);
        op(&mut self.declarations,43,&[UINT,id,n]);id
    }
    fn word(&mut self,i:&ir::Inst,o:Operand)->Result<u32,ShaderError> {
        match o {
            Operand::Vector{reg,dwords:1}=>{
                let r=Reg::V(reg);
                if !self.defined.contains(&r){return Err(err(i.machine.offset,"undefined graphics operand"));}
                let v=i.read(r).ok_or(err(i.machine.offset,"missing graphics operand"))?;
                self.values.get(&v).copied().ok_or(err(i.machine.offset,"missing graphics SSA value"))
            }
            Operand::Literal(n)=>Ok(self.constant(n)),
            Operand::Const(n @ 128..=192)=>Ok(self.constant(u32::from(n-128))),
            Operand::Const(n @ 193..=208)=>Ok(self.constant(0u32.wrapping_sub(u32::from(n-192)))),
            Operand::Const(n @ 240..=247)=>{
                const FLOATS:[f32;8]=[0.5,-0.5,1.,-1.,2.,-2.,4.,-4.];
                Ok(self.constant(FLOATS[(n-240) as usize].to_bits()))
            }
            _=>Err(err(i.machine.offset,"unsupported graphics operand")),
        }
    }
    fn float(&mut self,a:u32)->u32 {self.value(124,FLOAT,&[a])}
    fn fp(&mut self,opcode:u16,a:u32,b:u32)->u32 {
        let a=self.float(a);let b=self.float(b);let f=self.value(opcode,FLOAT,&[a,b]);
        self.value(124,UINT,&[f])
    }
}

pub fn emit_graphics(p:&ir::Program,stage:Stage)->Result<Vec<u32>,ShaderError> {
    if p.blocks.len()!=1 || p.vgprs>64 {return Err(err(0,"graphics CFG or register count"));}
    let b=&p.blocks[0];
    if !b.preds.is_empty()||!b.phis.is_empty()||b.exit!=ir::Exit::End||b.insts.len()>4096 {
        return Err(err(b.offset,"graphics control flow"));
    }
    let mut e=Emit{next:16,constants:BTreeMap::new(),declarations:vec![],body:vec![],
        values:BTreeMap::new(),defined:BTreeSet::new()};
    op(&mut e.body,54,&[VOID,MAIN,0,FN]);op(&mut e.body,248,&[ENTRY]);
    let first=e.value(61,if stage==Stage::Vertex{VEC3}else{VEC4},&[INPUT0]);
    let second=if stage==Stage::Vertex{Some(e.value(61,VEC4,&[INPUT1]))}else{None};
    let mut masks=BTreeMap::new();
    for &(r,v) in &p.inputs {
        let (vector,ch)=match r {
            Reg::V(n @ 0..=2) if stage==Stage::Vertex=>(first,n),
            Reg::V(n @ 3..=6) if stage==Stage::Vertex=>(second.ok_or(err(0,"missing vertex input"))?,n-3),
            Reg::V(n @ 0..=3) if stage==Stage::Fragment=>(first,n),
            Reg::S(n) if n==EXEC||(p.wave64&&n==EXEC+1)=>{masks.insert(r,v);continue;},
            Reg::V(_) => {
                // SSA reads an old VGPR even for writes under full EXEC.
                // Never allow this undefined entry value as an actual operand.
                continue;
            }
            _=>return Err(err(0,"unsupported graphics input")),
        };
        let f=e.value(81,FLOAT,&[vector,u32::from(ch)]);
        let word=e.value(124,UINT,&[f]);
        e.values.insert(v,word);e.defined.insert(r);
    }
    let mut exports=BTreeSet::new();
    let mut done=false;
    for i in &b.insts {
        let reject=||err(i.machine.offset,"unsupported graphics instruction or export");
        if i.name()=="s_endpgm"&&i.reads.is_empty()&&i.writes.is_empty(){continue;}
        if done{return Err(reject());}
        for r in [Reg::S(EXEC),Reg::S(EXEC+1)].into_iter().take(if p.wave64{2}else{1}){
            if masks.get(&r).copied().is_none()||i.read(r)!=masks.get(&r).copied(){return Err(reject());}
        }
        if i.reads.iter().any(|(r,_)|!matches!(r,Reg::V(0..=63))&&!masks.contains_key(r))
            ||i.writes.iter().any(|(r,_)|!matches!(r,Reg::V(0..=63))){return Err(reject());}
        match &i.fields {
            Fields::Valu(v)=>{
                if !matches!(i.machine.encoding,Encoding::Vop1|Encoding::Vop2)
                    ||v.carry.is_some()||v.neg!=0||v.abs!=0||v.neg_hi!=0||v.opsel!=0||v.opsel_hi!=0
                    ||v.clamp||v.omod!=0||v.sdwa.is_some()||v.dpp.is_some(){return Err(reject());}
                let Some(Operand::Vector{reg,dwords:1})=v.dst else{return Err(reject());};
                let r=Reg::V(reg);
                if i.writes.len()!=1{return Err(reject());}
                let value=i.write(r).ok_or_else(reject)?;
                let args:Vec<_>=v.src.iter().map(|o|e.word(i,*o)).collect::<Result<_,_>>()?;
                let word=match (i.name(),args.as_slice()){
                    ("v_mov_b32",&[a])=>a,
                    ("v_add_f32",&[a,b])=>e.fp(129,a,b),
                    ("v_sub_f32",&[a,b])=>e.fp(131,a,b),
                    ("v_subrev_f32",&[a,b])=>e.fp(131,b,a),
                    ("v_mul_f32",&[a,b])=>e.fp(133,a,b),
                    ("v_add_nc_u32",&[a,b])=>e.value(128,UINT,&[a,b]),
                    ("v_sub_nc_u32",&[a,b])=>e.value(130,UINT,&[a,b]),
                    ("v_and_b32",&[a,b])=>e.value(199,UINT,&[a,b]),
                    ("v_or_b32",&[a,b])=>e.value(197,UINT,&[a,b]),
                    ("v_xor_b32",&[a,b])=>e.value(198,UINT,&[a,b]),
                    _=>return Err(reject()),
                };
                if e.values.insert(value,word).is_some(){return Err(reject());}
                e.defined.insert(r);
            }
            Fields::Exp{target,src,compr,done:finish,vm:_}=>{
                if *compr||!i.writes.is_empty()||!exports.insert(*target){return Err(reject());}
                let output=match (stage,*target){
                    (Stage::Vertex,12)=>POSITION,
                    (Stage::Vertex,32)|(Stage::Fragment,0)=>OUTPUT0,
                    _=>return Err(reject()),
                };
                let mut components=Vec::new();
                for source in src {
                    let word=e.word(i,source.ok_or_else(reject)?)?;
                    components.push(e.float(word));
                }
                let v=e.value(80,VEC4,&components);op(&mut e.body,62,&[output,v]);
                done=*finish;
            }
            _=>return Err(reject()),
        }
    }
    let required=if stage==Stage::Vertex{BTreeSet::from([12,32])}else{BTreeSet::from([0])};
    if !done||exports!=required{return Err(err(0,"missing required graphics exports"));}
    op(&mut e.body,253,&[]);op(&mut e.body,56,&[]);
    let mut out=vec![0x07230203,0x00010300,0,e.next,0];
    op(&mut out,17,&[1]);op(&mut out,14,&[0,1]);
    let mut entry=vec![if stage==Stage::Vertex{0}else{4},MAIN,0x6e69616d,0,INPUT0,OUTPUT0];
    if stage==Stage::Vertex{entry.extend([INPUT1,POSITION]);}
    op(&mut out,15,&entry);
    if stage==Stage::Fragment{op(&mut out,16,&[MAIN,7]);} // OriginUpperLeft
    op(&mut out,71,&[INPUT0,30,0]);op(&mut out,71,&[OUTPUT0,30,0]);
    if stage==Stage::Vertex{op(&mut out,71,&[INPUT1,30,1]);op(&mut out,71,&[POSITION,11,0]);}
    op(&mut out,19,&[VOID]);op(&mut out,33,&[FN,VOID]);op(&mut out,21,&[UINT,32,0]);op(&mut out,22,&[FLOAT,32]);
    op(&mut out,23,&[VEC3,FLOAT,3]);op(&mut out,23,&[VEC4,FLOAT,4]);
    for (id,storage,ty) in [(IN3,1,VEC3),(IN4,1,VEC4),(OUT4,3,VEC4)]{op(&mut out,32,&[id,storage,ty]);}
    out.extend(e.declarations);
    op(&mut out,59,&[if stage==Stage::Vertex{IN3}else{IN4},INPUT0,1]);
    op(&mut out,59,&[OUT4,OUTPUT0,3]);
    if stage==Stage::Vertex{op(&mut out,59,&[IN4,INPUT1,1]);op(&mut out,59,&[OUT4,POSITION,3]);}
    out.extend(e.body);Ok(out)
}

/// Original fixtures. Vertex halves RGB, fragment adds 0.25 to RGB.
/// Alpha and depth remain unchanged. Both use actual decoded EXP words.
pub fn fixtures()->(Vec<u8>,Vec<u8>){
    let encode=|w:&[u32]|w.iter().flat_map(|n|n.to_le_bytes()).collect();
    let vertex=encode(&[
        0x7e0e_02f2, // v7=1.0
        0x1006_06f0,0x1008_08f0,0x100a_0af0, // RGB *= 0.5
        0xf800_00cf,0x0702_0100, // POS0
        0xf800_0a0f,0x0605_0403, // PARAM0 done
        0xbf81_0000,
    ]);
    let fragment=encode(&[
        0x0600_00ff,0x3e80_0000,0x0602_02ff,0x3e80_0000,0x0604_04ff,0x3e80_0000,
        0xf800_080f,0x0302_0100,0xbf81_0000,
    ]);
    (vertex,fragment)
}
pub fn fixture_modules()->Result<(Vec<u32>,Vec<u32>),ShaderError>{
    let (v,f)=fixtures();
    Ok((emit_graphics(&ir::build(&v,false,8)?,Stage::Vertex)?,
        emit_graphics(&ir::build(&f,false,4)?,Stage::Fragment)?))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_stage_fixtures_emit(){
        let (v,f)=fixture_modules().unwrap();
        assert_eq!(v[0],0x07230203);assert_eq!(f[0],0x07230203);
    }
    #[test]
    fn wrong_stage_rejected(){
        let (v,_)=fixtures();
        assert!(emit_graphics(&ir::build(&v,false,8).unwrap(),Stage::Fragment).is_err());
    }
}

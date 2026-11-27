// SPDX-License-Identifier: GPL-3.0-or-later
//! Initial graphics stage lowering with an explicit test ABI.
//! Vertex v0..v2 = location 0 xyz, v3..v6 = location 1 rgba.
//! Pixel v0..v3 = interpolated location 0 rgba. This is NOT the console
//! fetch/interpolation ABI; no inference of native shader signatures yet.
//! Full EXEC, straight line, POS0/PARAM0 or MRT0 and optional depth export.
//! A restricted native P1/P2 interpolation pair maps attr0 to location 0;
//! normalized 2D MIMG sampling maps s[4:11]/s[12:15] to combined binding 0.
//! Supports normalized RGBA sampling and partial dmask, including explicit LOD.
//! FP32 ALU assumes finite ordinary values; exact guest FP mode/NaN rules are
//! not yet modeled. Reject unsupported effects.
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
const VEC2:u32=16;
const IMAGE:u32=17;
const SAMPLED:u32=18;
const TEX_PTR:u32=19;
const TEX:u32=20;
const DEPTH_PTR:u32=21;
const DEPTH:u32=22;
const GLSL:u32=23;

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
    fn ext_fp(&mut self,opcode:u32,args:&[u32])->u32 {
        let mut operands=vec![GLSL,opcode];
        for &arg in args {operands.push(self.float(arg));}
        let result=self.value(12,FLOAT,&operands);
        self.value(124,UINT,&[result])
    }
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
    let sampled=b.insts.iter().any(|i|matches!(i.fields,Fields::Image{..}));
    let interp=b.insts.iter().any(|i|matches!(i.fields,Fields::Vintrp{..}));
    let depth=b.insts.iter().any(|i|matches!(i.fields,Fields::Exp{target:8,..}));
    if stage!=Stage::Fragment&&(sampled||interp||depth){return Err(err(0,"fragment-only graphics effect"));}
    let mut e=Emit{next:24,constants:BTreeMap::new(),declarations:vec![],body:vec![],
        values:BTreeMap::new(),defined:BTreeSet::new()};
    op(&mut e.body,54,&[VOID,MAIN,0,FN]);op(&mut e.body,248,&[ENTRY]);
    let first=e.value(61,if stage==Stage::Vertex{VEC3}else{VEC4},&[INPUT0]);
    let second=if stage==Stage::Vertex{Some(e.value(61,VEC4,&[INPUT1]))}else{None};
    let mut masks=BTreeMap::new();
    let entry_value=|reg:Reg|p.inputs.iter().find(|(r,_)|*r==reg).map(|(_,v)|*v);
    for &(r,v) in &p.inputs {
        let (vector,ch)=match r {
            Reg::V(n @ 0..=2) if stage==Stage::Vertex=>(first,n),
            Reg::V(n @ 3..=6) if stage==Stage::Vertex=>(second.ok_or(err(0,"missing vertex input"))?,n-3),
            Reg::V(n @ 0..=3) if stage==Stage::Fragment&&!interp=>(first,n),
            Reg::S(124) if interp=>continue,
            Reg::S(4..=15) if sampled=>continue,
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
    let mut halves=BTreeMap::<Value,(u8,u8)>::new();
    let mut exports=BTreeSet::new();
    let mut done=false;
    for i in &b.insts {
        let reject=||err(i.machine.offset,"unsupported graphics instruction or export");
        if i.name()=="s_endpgm"&&i.reads.is_empty()&&i.writes.is_empty(){continue;}
        if done{return Err(reject());}
        for r in [Reg::S(EXEC),Reg::S(EXEC+1)].into_iter().take(if p.wave64{2}else{1}){
            if masks.get(&r).copied().is_none()||i.read(r)!=masks.get(&r).copied(){return Err(reject());}
        }
        if i.reads.iter().any(|(r,_)|!matches!(r,Reg::V(0..=63))&&!masks.contains_key(r)
            && !(matches!(i.fields,Fields::Vintrp{..})&&*r==Reg::S(124))
            && !(matches!(i.fields,Fields::Image{..})&&matches!(r,Reg::S(4..=15))))
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
                    ("v_floor_f32",&[a])=>e.ext_fp(8,&[a]),
                    ("v_ceil_f32",&[a])=>e.ext_fp(9,&[a]),
                    ("v_fract_f32",&[a])=>e.ext_fp(10,&[a]),
                    ("v_trunc_f32",&[a])=>e.ext_fp(3,&[a]),
                    ("v_exp_f32",&[a])=>e.ext_fp(29,&[a]),
                    ("v_log_f32",&[a])=>e.ext_fp(30,&[a]),
                    ("v_sqrt_f32",&[a])=>e.ext_fp(31,&[a]),
                    ("v_rsq_f32",&[a])=>e.ext_fp(32,&[a]),
                    ("v_rcp_f32",&[a])=>{
                        let one=e.constant(1f32.to_bits());e.fp(136,one,a)
                    }
                    ("v_min_f32",&[a,b])=>e.ext_fp(37,&[a,b]),
                    ("v_max_f32",&[a,b])=>e.ext_fp(40,&[a,b]),
                    ("v_cvt_f32_u32",&[a])=>{
                        let f=e.value(112,FLOAT,&[a]);e.value(124,UINT,&[f])
                    }
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
            Fields::Vintrp{dst,src,attr}=>{
                let Operand::Vector{reg,dwords:1}=*dst else{return Err(reject());};
                let Operand::Attr{index:0,chan,high:false}=*attr else{return Err(reject());};
                let r=Reg::V(reg);
                if i.writes.len()!=1{return Err(reject());}
                let result=i.write(r).ok_or_else(reject)?;
                match (i.name(),*src) {
                    ("v_interp_p1_f32",Operand::Vector{reg:0,dwords:1})=>{
                        // Only the canonical entry barycentric pair is accepted.
                        if entry_value(Reg::V(0)).is_none()||i.read(Reg::V(0))!=entry_value(Reg::V(0))
                            ||reg<2||i.read(Reg::S(124))!=entry_value(Reg::S(124)){return Err(reject());}
                        // p1 is only a marker, never an independently usable value.
                        halves.insert(result,(reg,chan));e.defined.remove(&r);
                    }
                    ("v_interp_p2_f32",Operand::Vector{reg:1,dwords:1})=>{
                        if entry_value(Reg::V(1)).is_none()||i.read(Reg::V(1))!=entry_value(Reg::V(1))
                            ||i.read(Reg::S(124))!=entry_value(Reg::S(124)){return Err(reject());}
                        let old=i.read(r).ok_or_else(reject)?;
                        if halves.remove(&old)!=Some((reg,chan)){return Err(reject());}
                        let f=e.value(81,FLOAT,&[first,u32::from(chan)]);
                        let word=e.value(124,UINT,&[f]);e.values.insert(result,word);e.defined.insert(r);
                    }
                    _=>return Err(reject()),
                }
            }
            Fields::Image{data,addr,rsrc,sampler,dmask,dim,unorm,r128,a16,d16,tfe,lwe,cache}=>{
                let explicit=i.name()=="image_sample_l";
                if !(explicit||i.name()=="image_sample")||*dim!=1||*dmask==0||*dmask>15||*unorm||*r128
                    ||*a16||*d16||*tfe||*lwe||cache.glc||cache.slc||cache.dlc
                    ||*rsrc!=(Operand::Scalar{code:4,dwords:8})
                    ||*sampler!=Some(Operand::Scalar{code:12,dwords:4}){return Err(reject());}
                if (4..=15).any(|r|entry_value(Reg::S(r)).is_none()||i.read(Reg::S(r))!=entry_value(Reg::S(r))){return Err(reject());}
                let [Operand::Vector{reg:base,dwords:n}]=addr.as_slice() else{return Err(reject());};
                if *n!=(if explicit{3}else{2})||*base>61{return Err(reject());}
                let Operand::Vector{reg:dest,dwords}=*data else{return Err(reject());};
                let count=dmask.count_ones() as u8;
                if dwords!=count||u16::from(dest)+u16::from(count)>64||i.writes.len()!=usize::from(count){return Err(reject());}
                let u=e.word(i,Operand::Vector{reg:*base,dwords:1})?;
                let v=e.word(i,Operand::Vector{reg:*base+1,dwords:1})?;
                let u=e.float(u);let v=e.float(v);let uv=e.value(80,VEC2,&[u,v]);
                let tex=e.value(61,SAMPLED,&[TEX]);
                let rgba=if explicit{
                    let lod=e.word(i,Operand::Vector{reg:*base+2,dwords:1})?;
                    let lod=e.float(lod);e.value(88,VEC4,&[tex,uv,2,lod])
                }else{e.value(87,VEC4,&[tex,uv])};
                let mut packed=0;
                for ch in 0..4u8{
                    if *dmask&(1<<ch)==0{continue;}
                    let r=Reg::V(dest+packed);let result=i.write(r).ok_or_else(reject)?;
                    let f=e.value(81,FLOAT,&[rgba,u32::from(ch)]);let w=e.value(124,UINT,&[f]);
                    e.values.insert(result,w);e.defined.insert(r);packed+=1;
                }
            }
            Fields::Exp{target,src,compr,done:finish,vm:_}=>{
                if *compr||!i.writes.is_empty()||!exports.insert(*target){return Err(reject());}
                let output=match (stage,*target){
                    (Stage::Vertex,12)=>POSITION,
                    (Stage::Vertex,32)|(Stage::Fragment,0)=>OUTPUT0,
                    (Stage::Fragment,8)=>DEPTH,
                    _=>return Err(reject()),
                };
                if output==DEPTH {
                    if src[1..].iter().any(Option::is_some){return Err(reject());}
                    let word=e.word(i,src[0].ok_or_else(reject)?)?;
                    let f=e.float(word);op(&mut e.body,62,&[DEPTH,f]);done=*finish;continue;
                }
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
    let required=if stage==Stage::Vertex{BTreeSet::from([12,32])}else if depth{BTreeSet::from([0,8])}else{BTreeSet::from([0])};
    if !done||exports!=required||!halves.is_empty(){return Err(err(0,"missing required graphics exports"));}
    op(&mut e.body,253,&[]);op(&mut e.body,56,&[]);
    let mut out=vec![0x07230203,0x00010300,0,e.next,0];
    op(&mut out,17,&[1]);
    op(&mut out,11,&[GLSL,0x4c534c47,0x6474732e,0x3035342e,0]); // GLSL.std.450
    op(&mut out,14,&[0,1]);
    let mut entry=vec![if stage==Stage::Vertex{0}else{4},MAIN,0x6e69616d,0,INPUT0,OUTPUT0];
    if stage==Stage::Vertex{entry.extend([INPUT1,POSITION]);}
    if depth{entry.push(DEPTH);}
    op(&mut out,15,&entry);
    if stage==Stage::Fragment{op(&mut out,16,&[MAIN,7]);} // OriginUpperLeft
    if depth{op(&mut out,16,&[MAIN,12]);op(&mut out,71,&[DEPTH,11,22]);}
    if sampled{op(&mut out,71,&[TEX,34,0]);op(&mut out,71,&[TEX,33,0]);}
    op(&mut out,71,&[INPUT0,30,0]);op(&mut out,71,&[OUTPUT0,30,0]);
    if stage==Stage::Vertex{op(&mut out,71,&[INPUT1,30,1]);op(&mut out,71,&[POSITION,11,0]);}
    op(&mut out,19,&[VOID]);op(&mut out,33,&[FN,VOID]);op(&mut out,21,&[UINT,32,0]);op(&mut out,22,&[FLOAT,32]);
    op(&mut out,23,&[VEC3,FLOAT,3]);op(&mut out,23,&[VEC4,FLOAT,4]);
    for (id,storage,ty) in [(IN3,1,VEC3),(IN4,1,VEC4),(OUT4,3,VEC4)]{op(&mut out,32,&[id,storage,ty]);}
    if sampled{
        op(&mut out,23,&[VEC2,FLOAT,2]);op(&mut out,25,&[IMAGE,FLOAT,1,0,0,0,1,0]);
        op(&mut out,27,&[SAMPLED,IMAGE]);op(&mut out,32,&[TEX_PTR,0,SAMPLED]);
    }
    if depth{op(&mut out,32,&[DEPTH_PTR,3,FLOAT]);}
    out.extend(e.declarations);
    op(&mut out,59,&[if stage==Stage::Vertex{IN3}else{IN4},INPUT0,1]);
    op(&mut out,59,&[OUT4,OUTPUT0,3]);
    if stage==Stage::Vertex{op(&mut out,59,&[IN4,INPUT1,1]);op(&mut out,59,&[OUT4,POSITION,3]);}
    if sampled{op(&mut out,59,&[TEX_PTR,TEX,0]);}
    if depth{op(&mut out,59,&[DEPTH_PTR,DEPTH,3]);}
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
/// Explicit native interpolation of PARAM0.xy, followed by normalized 2D
/// image sampling. The optional explicit LOD is zero; native depth export
/// uses 0.25, independent of incoming vertex depth.
pub fn texture_fixtures(explicit:bool,depth:bool)->(Vec<u8>,Vec<u8>){
    let encode=|w:&[u32]|w.iter().flat_map(|n|n.to_le_bytes()).collect();
    let vertex=encode(&[0x7e0e_02f2,0xf800_00cf,0x0702_0100,
        0xf800_0a0f,0x0605_0403,0xbf81_0000]);
    let mut pixel=vec![0xc810_0000u32,0xc811_0001,0xc814_0100,0xc815_0101];
    if explicit{pixel.push(0x7e0c_0280);}
    pixel.extend([if explicit{0xf090_0f08}else{0xf080_0f08},0x0061_0804,0xbf8c_0000]);
    if depth{
        pixel.extend([0x7e18_02ff,0x3e80_0000,0xf800_000f,0x0b0a_0908,
            0xf800_0881,0x0000_000c]);
    }else{pixel.extend([0xf800_080f,0x0b0a_0908]);}
    pixel.push(0xbf81_0000);(vertex,encode(&pixel))
}
pub fn texture_modules(explicit:bool,depth:bool)->Result<(Vec<u32>,Vec<u32>),ShaderError>{
    let (v,f)=texture_fixtures(explicit,depth);
    Ok((emit_graphics(&ir::build(&v,false,8)?,Stage::Vertex)?,
        emit_graphics(&ir::build(&f,false,13)?,Stage::Fragment)?))
}
/// Original fixture with selectable integer mip LOD and optional packed RB
/// sample (dmask 0101). The missing channels are supplied by native VALU.
pub fn mip_fixtures(lod:u8,packed:bool)->(Vec<u8>,Vec<u8>){
    let (vertex,_)=texture_fixtures(true,false);
    let mut pixel=vec![0xc810_0000u32,0xc811_0001,0xc814_0100,0xc815_0101,
        0x7e0c_02ff,(lod as f32).to_bits(),
        if packed{0xf090_0508}else{0xf090_0f08},0x0061_0804,0xbf8c_0000];
    if packed{
        // v8 = R, v9 = B; output R, 0, B, 1.
        pixel.extend([0x7e14_0280,0x7e16_02f2,0xf800_080f,0x0b09_0a08]);
    }else{pixel.extend([0xf800_080f,0x0b0a_0908]);}
    pixel.push(0xbf81_0000);
    (vertex,pixel.into_iter().flat_map(u32::to_le_bytes).collect())
}

/// Test finite native FP32 unary/binary operations through actual pixel output.
/// v4=fract(1.25), v5=sqrt(0.25), v6=rcp(4), v7=min(1,ceil(0.25)).
pub fn math_fixtures()->(Vec<u8>,Vec<u8>){
    let (vertex,_)=texture_fixtures(false,false);
    let words=[0x7e08_02ffu32,1.25f32.to_bits(),0x7e08_4104,
        0x7e0a_02ff,0.25f32.to_bits(),0x7e0a_6705,
        0x7e0c_02f6,0x7e0c_5506,
        0x7e0e_02ff,0.25f32.to_bits(),0x7e0e_4507,0x1e0e_0ef2,
        0xf800_080f,0x0706_0504,0xbf81_0000];
    (vertex,words.into_iter().flat_map(u32::to_le_bytes).collect())
}
pub fn extra_modules(v:&[u8],f:&[u8])->Result<(Vec<u32>,Vec<u32>),ShaderError>{
    Ok((emit_graphics(&ir::build(v,false,8)?,Stage::Vertex)?,
        emit_graphics(&ir::build(f,false,13)?,Stage::Fragment)?))
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
    fn texture_and_depth_emit(){
        for explicit in [false,true]{for depth in [false,true]{assert!(texture_modules(explicit,depth).is_ok());}}
    }
    #[test]
    fn additional_graphics_modules_emit(){
        for lod in 0..3{for packed in [false,true]{
            let (v,f)=mip_fixtures(lod,packed);assert!(extra_modules(&v,&f).is_ok());
        }}
        let (v,f)=math_fixtures();assert!(extra_modules(&v,&f).is_ok());
    }
    #[test]
    fn wrong_stage_rejected(){
        let (v,_)=fixtures();
        assert!(emit_graphics(&ir::build(&v,false,8).unwrap(),Stage::Fragment).is_err());
    }
}

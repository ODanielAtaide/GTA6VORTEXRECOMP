// SPDX-License-Identifier: GPL-3.0-or-later
//! Pointer-free private root signature encoding for the live Vulkan ABI.
//! This is not a DXBC/RTS0 blob and is not exported as one. A root's
//! binding metadata is preserved, but pipeline layout lowering is separate.
use super::{Desc,c_void,object,This,Object,Table,Iid,HRESULT,ROOT_SIGNATURE,guid};
use crate::object::{S_OK,E_INVALIDARG,E_NOINTERFACE,E_POINTER,E_NOTIMPL};
use windows_sys::Win32::System::{Diagnostics::Debug::ReadProcessMemory,Threading::GetCurrentProcess};

const MAGIC:u32=u32::from_le_bytes(*b"VRS1");
const LIMIT:usize=1_048_576;
const UNKNOWN:Iid=guid(0,0,0,[0xc0,0,0,0,0,0,0,0x46]);
const BLOB:Iid=guid(0x8ba5fb08,0x5195,0x40e2,[0xac,0x58,0x0d,0x98,0x9c,0x3a,0x01,0x02]);
const DESERIALIZER:Iid=guid(0x34ab647b,0x3cc8,0x46ac,[0x84,0x1b,0xc0,0x96,0x56,0x45,0xc0,0x46]);
type R<T>=Result<T,&'static str>;

fn guest_words(address:usize,n:usize)->R<Vec<u32>>{
    if n==0{return Ok(Vec::new());}
    if address==0||n>LIMIT{return Err("invalid root signature array");}
    let bytes=n.checked_mul(4).ok_or("root array overflow")?;
    address.checked_add(bytes).ok_or("root address overflow")?;
    let mut words=vec![0u32;n];let mut actual=0usize;
    // SAFETY: own process, kernel verifies readability, destination is owned.
    if unsafe{ReadProcessMemory(GetCurrentProcess(),address as *const _,words.as_mut_ptr().cast(),bytes,&mut actual)}==0||actual!=bytes{
        return Err("root signature array is unreadable");
    }
    Ok(words)
}
fn range_stride(version:u32)->R<usize>{
    match version{1=>Ok(5),2|3=>Ok(6),_=>Err("unsupported root signature version")}
}
fn sampler_stride(version:u32)->R<usize>{
    match version{1|2=>Ok(13),3=>Ok(14),_=>Err("unsupported root signature version")}
}
fn append(out:&mut Vec<u32>,words:&[u32])->R<()>{
    if out.len().checked_add(words.len()).is_none_or(|n|n>LIMIT){return Err("root signature too large");}
    out.extend_from_slice(words);Ok(())
}
fn encode(desc:Desc,version:u32)->R<Vec<u32>>{
    let rs=range_stride(version)?;let ss=sampler_stride(version)?;
    let pc=desc.parameter_count as usize;let sc=desc.sampler_count as usize;
    if pc>1024||sc>4096{return Err("root signature count exceeds limit");}
    // x64 guest layout: type at 0, union at 8, shader visibility at 24.
    // The descriptor table contains count at 8 and a range pointer at 16.
    let params=guest_words(desc.parameters as usize,pc*8)?;
    let mut words=vec![MAGIC,version,desc.flags,desc.parameter_count,desc.sampler_count];
    for p in params.chunks_exact(8){
        let kind=p[0];let vis=p[6];
        match kind{
            0=>{
                let n=p[2] as usize;
                let address=(u64::from(p[4])|(u64::from(p[5])<<32)) as usize;
                let ranges=guest_words(address,n.checked_mul(rs).ok_or("range count overflow")?)?;
                if ranges.chunks_exact(rs).any(|v|v[0]>3){return Err("unsupported descriptor range type");}
                append(&mut words,&[kind,vis,p[2],0,0])?;
                append(&mut words,&ranges)?;
            }
            1=>append(&mut words,&[kind,vis,p[2],p[3],p[4]])?,
            2..=4=>append(&mut words,&[kind,vis,p[2],p[3],if version==1{0}else{p[4]}])?,
            _=>return Err("unsupported root parameter type"),
        }
    }
    append(&mut words,&guest_words(desc.samplers as usize,sc*ss)?)?;
    validate(&words)?;
    Ok(words)
}
fn take<'a>(words:&mut &'a [u32],n:usize)->R<&'a [u32]>{
    if n>words.len(){return Err("truncated private root signature");}
    let (first,rest)=words.split_at(n);*words=rest;Ok(first)
}
fn validate(mut words:&[u32])->R<()>{
    if words.len()>LIMIT{return Err("root signature too large");}
    let h=take(&mut words,5)?;
    if h[0]!=MAGIC{return Err("not a private root signature");}
    let rs=range_stride(h[1])?;let ss=sampler_stride(h[1])?;
    if h[3]>1024||h[4]>4096{return Err("root count exceeds limit");}
    for _ in 0..h[3]{
        let p=take(&mut words,5)?;
        match p[0]{
            0=>{
                let ranges=take(&mut words,(p[2] as usize).checked_mul(rs).ok_or("range overflow")?)?;
                if ranges.chunks_exact(rs).any(|r|r[0]>3){return Err("invalid descriptor range type");}
            }
            1..=4=>{},
            _=>return Err("invalid root parameter type"),
        }
    }
    take(&mut words,(h[4] as usize)*ss)?;
    if !words.is_empty(){return Err("trailing private root signature data");}
    Ok(())
}
fn decode(bytes:&[u8])->R<Vec<u32>>{
    if bytes.len()%4!=0||bytes.len()/4>LIMIT{return Err("invalid root blob size");}
    let words:Vec<_>=bytes.chunks_exact(4).map(|c|u32::from_le_bytes([c[0],c[1],c[2],c[3]])).collect();
    validate(&words)?;Ok(words)
}
pub(super) fn inspect(bytes:&[u8])->R<()>{
    if bytes.starts_with(b"DXBC"){
        // Keep externally supplied standard blobs opaque until RTS0 decoding
        // is implemented. They are not usable as translated pipeline layouts.
        tracing::debug!("external root signature retained, RTS0 lowering pending");
        return Ok(());
    }
    let w=decode(bytes)?;
    tracing::debug!(version=w[1],flags=w[2],parameters=w[3],samplers=w[4],"native root metadata retained");
    Ok(())
}
#[repr(C)]
struct Blob{object:Object,bytes:Vec<u8>}
fn blob(data:Vec<u8>)->*mut c_void{
    let p=Box::into_raw(Box::new(Blob{object:Object::new(&BLOB_TABLE),bytes:data})).cast::<Object>();
    object::add_ref(p);p.cast()
}
extern "system" fn blob_query(this:This,iid:*const Iid,out:*mut This)->HRESULT{
    object::answer(ROOT_SIGNATURE,iid,object::is(iid,&[UNKNOWN,BLOB]).then_some(this),out)
}
extern "system" fn blob_release(this:This)->u32{object::release_boxed::<Blob>(this)}
extern "system" fn blob_data(this:This)->*mut c_void{unsafe{(&*this.cast::<Blob>()).bytes.as_ptr().cast_mut().cast()}}
extern "system" fn blob_size(this:This)->usize{unsafe{(&*this.cast::<Blob>()).bytes.len()}}
static BLOB_TABLE:Table<5>=Table([blob_query as *const (),object::add_ref as *const (),blob_release as *const (),blob_data as *const (),blob_size as *const ()]);
pub(super) fn serialize(desc:Desc,version:u32,out:*mut *mut c_void,error:*mut *mut c_void)->HRESULT{
    if out.is_null(){return E_POINTER;}
    // SAFETY: ABI outputs are caller-owned slots.
    unsafe{out.write(std::ptr::null_mut());if !error.is_null(){error.write(std::ptr::null_mut());}}
    match encode(desc,version){
        Ok(words)=>{
            let data=words.into_iter().flat_map(u32::to_le_bytes).collect();
            unsafe{out.write(blob(data));}
            static FIRST:std::sync::Once=std::sync::Once::new();
            FIRST.call_once(||println!("VULKAN_GUEST_ROOT_SERIALIZED: version={version}, parameters={}, samplers={}, flags={:#x}",desc.parameter_count,desc.sampler_count,desc.flags));
            S_OK
        }
        Err(e)=>{
            tracing::error!(version,reason=e,"native root signature serialization failed");
            if !error.is_null(){let mut msg=e.as_bytes().to_vec();msg.push(0);unsafe{error.write(blob(msg));}}
            E_INVALIDARG
        }
    }
}
// The unversioned legacy deserializer is supported only for private 1.0
// metadata. A 1.1/1.2 downgrade would discard resource/sampler flags.
#[repr(C)]
struct Parameter{kind:u32,_padding:u32,payload:[u64;2],visibility:u32,_padding2:u32}
const _:()=assert!(std::mem::size_of::<Parameter>()==32);
#[repr(C)]
struct Deserialized{object:Object,desc:Desc,_params:Vec<Parameter>,_ranges:Vec<Vec<u32>>,_samplers:Vec<u32>}
extern "system" fn des_query(this:This,iid:*const Iid,out:*mut This)->HRESULT{
    object::answer(ROOT_SIGNATURE,iid,object::is(iid,&[UNKNOWN,DESERIALIZER]).then_some(this),out)
}
extern "system" fn des_release(this:This)->u32{object::release_boxed::<Deserialized>(this)}
extern "system" fn des_desc(this:This)->*const Desc{unsafe{&(*this.cast::<Deserialized>()).desc}}
static DES_TABLE:Table<4>=Table([des_query as *const (),object::add_ref as *const (),des_release as *const (),des_desc as *const ()]);
fn restore(words:&[u32])->R<Box<Deserialized>>{
    validate(words)?;
    if words[1]!=1{return Err("legacy root deserializer requires version 1.0");}
    let mut cursor=&words[5..];let mut params=Vec::new();let mut ranges=Vec::new();
    for _ in 0..words[3]{
        let p=take(&mut cursor,5)?;
        let payload=if p[0]==0{
            let data=take(&mut cursor,p[2] as usize*5)?.to_vec();
            let address=if data.is_empty(){0}else{data.as_ptr() as u64};
            ranges.push(data);[u64::from(p[2]),address]
        }else{[u64::from(p[2])|(u64::from(p[3])<<32),u64::from(p[4])]};
        params.push(Parameter{kind:p[0],_padding:0,payload,visibility:p[1],_padding2:0});
    }
    let samplers=cursor.to_vec();
    let desc=Desc{parameter_count:words[3],parameters:if params.is_empty(){std::ptr::null()}else{params.as_ptr().cast()},
        sampler_count:words[4],samplers:if samplers.is_empty(){std::ptr::null()}else{samplers.as_ptr().cast()},flags:words[2]};
    Ok(Box::new(Deserialized{object:Object::new(&DES_TABLE),desc,_params:params,_ranges:ranges,_samplers:samplers}))
}
pub(super) fn deserialize(data:*const u8,size:usize,iid:*const Iid,out:*mut *mut c_void)->HRESULT{
    if out.is_null(){return E_POINTER;}
    unsafe{out.write(std::ptr::null_mut());}
    if data.is_null()||size%4!=0||size/4>LIMIT{return E_INVALIDARG;}
    if !object::is(iid,&[UNKNOWN,DESERIALIZER]){return E_NOINTERFACE;}
    let result=guest_words(data as usize,size/4).and_then(|w|restore(&w));
    match result{
        Ok(value)=>{let p=Box::into_raw(value).cast::<Object>();object::add_ref(p);unsafe{out.write(p.cast());}S_OK}
        Err(e)=>{tracing::error!(reason=e,"native root deserialization unsupported");E_NOTIMPL}
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn empty_versioned_roots(){
        let d=Desc{parameter_count:0,parameters:std::ptr::null(),sampler_count:0,samplers:std::ptr::null(),flags:0x02000001};
        for v in 1..=3{let w=encode(d,v).unwrap();assert_eq!(w,[MAGIC,v,d.flags,0,0]);}
    }
    #[test]fn descriptor_table_roundtrip(){
        let ranges=[0u32,12,4,1,0xffff_ffff];
        let p=[Parameter{kind:0,_padding:0,payload:[1,ranges.as_ptr() as u64],visibility:5,_padding2:0},
            Parameter{kind:1,_padding:0,payload:[2|(3u64<<32),8],visibility:0,_padding2:0}];
        let d=Desc{parameter_count:2,parameters:p.as_ptr().cast(),sampler_count:0,samplers:std::ptr::null(),flags:1};
        let w=encode(d,1).unwrap();let restored=restore(&w).unwrap();
        assert_eq!(encode(restored.desc,1).unwrap(),w);
        assert!(validate(&w[..w.len()-1]).is_err());
        let mut bad=w.clone();bad.push(0);assert!(validate(&bad).is_err());
    }
    #[test]fn versioned_descriptor_flags_preserved(){
        let ranges=[1u32,5,7,2,4,0xffff_ffff];
        let p=Parameter{kind:0,_padding:0,payload:[1,ranges.as_ptr() as u64],visibility:0,_padding2:0};
        let d=Desc{parameter_count:1,parameters:(&raw const p).cast(),sampler_count:0,samplers:std::ptr::null(),flags:0x10000000};
        let w=encode(d,2).unwrap();assert_eq!(&w[10..],&ranges);assert!(restore(&w).is_err());
    }
}

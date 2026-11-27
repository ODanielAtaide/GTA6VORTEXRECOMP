// SPDX-License-Identifier: GPL-3.0-or-later
//! Guest pipeline descriptions for the live Vulkan renderer. Creation retains
//! the original metadata, not a fabricated VkPipeline. Lowering is attempted
//! only against real shader inputs. Dispatch is forbidden until the actual ABI
//! (resources, system IDs, extension state) has been implemented.
use super::{Bytecode,ComputeHead,GraphicsHead,StreamDesc,GRAPHICS_STAGES,PIPELINE_IIDS,subobject};
use crate::object::{self,This,Object,Table,Iid,HRESULT,PIPELINE,E_INVALIDARG};
use std::sync::{Arc,Mutex,OnceLock};
use std::collections::BTreeMap;
use vortex_shader::{Container,FourCC,HwStage,NativeProgram};
use windows_sys::Win32::System::{Diagnostics::Debug::ReadProcessMemory,Threading::GetCurrentProcess};

const LIMIT:usize=32*1024*1024;
fn read(address:usize,n:usize)->Result<Vec<u8>,String>{
    if n==0{return Ok(vec![]);}
    if address==0||n>LIMIT||address.checked_add(n).is_none(){return Err("invalid pipeline data range".into());}
    let mut bytes=vec![0;n];let mut actual=0usize;
    if unsafe{ReadProcessMemory(GetCurrentProcess(),address as *const _,bytes.as_mut_ptr().cast(),n,&mut actual)}==0||actual!=n{
        return Err("unreadable pipeline data".into());
    }
    Ok(bytes)
}

struct Stage{
    name:&'static str,hash:String,bytes:Vec<u8>,
}
impl Stage{
    fn new(name:&'static str,code:&Bytecode)->Result<Option<Self>,String>{
        if code.size==0{return Ok(None);}
        let bytes=read(code.code as usize,code.size)?;
        let hash=blake3::hash(&bytes).to_hex().as_str()[..16].to_owned();
        Ok(Some(Self{name,hash,bytes}))
    }
    fn analyze(&self)->Result<Vec<NativeProgram>,String>{
        let c=Container::parse(&self.bytes).map_err(|e|e.to_string())?;
        let part=c.part(FourCC::DXIL).ok_or("missing DXIL container part")?;
        let programs=vortex_shader::native_programs(part.data).map_err(|e|e.to_string())?;
        if programs.is_empty(){return Err("no native GFX10 programs".into());}
        for p in &programs{
            tracing::info!(stage=self.name,hash=%self.hash,hardware_stage=?p.stage,wave64=p.wave64,vgprs=p.vgprs,
                code_bytes=p.code.len(),layout=?p.layout,resources=?p.used,io=?p.io,"VULKAN_GUEST_SHADER_ABI");
            match vortex_shader::ir::build(&p.code,p.wave64,p.vgprs){
                Ok(ir)=>{
                    let ops:usize=ir.blocks.iter().map(|b|b.insts.len()).sum();
                    let inputs:Vec<_>=ir.inputs.iter().map(|(reg,_)|*reg).collect();
                    tracing::info!(hash=%self.hash,blocks=ir.blocks.len(),ops,?inputs,"VULKAN_GUEST_SHADER_IR");
                    if let vortex_shader::native::StageIo::Cs(io)=&p.io{
                        let (group_ids,group_info)=io.system_sgprs();
                        tracing::info!(hash=%self.hash,threads=?io.threads,user_sgprs=io.user_sgprs(),
                            ?group_ids,?group_info,"VULKAN_COMPUTE_HARDWARE_INPUTS");
                    }
                    // Diagnostic emitter has a synthetic ABI. Probe it for
                    // coverage only; its output must NEVER be executed here.
                    if p.stage==HwStage::Cs{
                        // Inventory all instruction-local failures in one pass.
                        // This does not bypass the real whole-program ABI checks.
                        if std::env::var_os("VORTEX_BATCH_DIAGNOSTICS").is_some_and(|v|v=="1"){
                            for (gap,count) in vortex_shader::spirv_cfg::diagnose_instructions(&ir){
                                tracing::info!(hash=%self.hash,%gap,count,"VULKAN_SHADER_BATCH_GAP");
                            }
                        }
                        match vortex_shader::spirv_cfg::emit_cfg(&ir){
                            Ok(words)=>tracing::info!(hash=%self.hash,words=words.len(),"VULKAN_SHADER_LOWERING_CANDIDATE_ONLY"),
                            Err(e)=>{
                                tracing::info!(hash=%self.hash,error=%e,"VULKAN_SHADER_LOWERING_GAP");
                                if let vortex_shader::ShaderError::Unsupported{offset,..}|vortex_shader::ShaderError::Isa{offset,..}=e{
                                    if let Some(i)=ir.blocks.iter().flat_map(|b|&b.insts).find(|i|i.machine.offset==offset){
                                        let reads:Vec<_>=i.reads.iter().map(|(r,_)|*r).collect();
                                        let writes:Vec<_>=i.writes.iter().map(|(r,_)|*r).collect();
                                        tracing::info!(hash=%self.hash,offset=format_args!("{offset:#x}"),opcode=i.name(),?reads,?writes,"VULKAN_SHADER_OPCODE_GAP");
                                    }
                                }
                            },
                        }
                    }
                }
                Err(e)=>tracing::info!(hash=%self.hash,error=%e,"VULKAN_SHADER_IR_GAP"),
            }
        }
        Ok(programs)
    }
}

pub(crate) struct Description{
    stages:Vec<Stage>,
    root:Option<Arc<[u8]>>,
    extension_count:u32,
    // Preserve known pipeline-stream state as bytes; do not interpret it
    // using the host D3D12 graphics state converter.
    stream:Option<Vec<u8>>,
    analysis:OnceLock<()>,
}
impl Description{
    fn analyze(&self){
        self.analysis.get_or_init(||{
            tracing::info!(stages=self.stages.len(),root_bytes=self.root.as_ref().map_or(0,|r|r.len()),
                extensions=self.extension_count,stream_bytes=self.stream.as_ref().map_or(0,|s|s.len()),
                "VULKAN_GUEST_PIPELINE_ANALYSIS");
            for stage in &self.stages{
                if let Err(e)=stage.analyze(){tracing::warn!(stage=stage.name,hash=%stage.hash,error=%e,"VULKAN_SHADER_CONTAINER_GAP");}
            }
        });
    }
    pub(crate) fn require_execution(&self,kind:&str)->!{
        self.analyze();
        // A decoded or even lowerable shader is not an executable guest PSO
        // until its root bindings and hardware inputs are actually mapped.
        let hashes=self.stages.iter().map(|s|format!("{}:{}",s.name,s.hash)).collect::<Vec<_>>().join(",");
        crate::vulkan_live::stop(format_args!("{kind}: native pipeline ABI not mapped ({hashes}, extensions={})",self.extension_count))
    }
}
#[repr(C)]
struct Pipeline{object:Object,packet:[u32;8],description:Arc<Description>}
const _:()=assert!(std::mem::offset_of!(Pipeline,packet)==0x10);
static LIVE:Mutex<BTreeMap<usize,Arc<Description>>>=Mutex::new(BTreeMap::new());
pub(crate) fn get(this:This)->Option<Arc<Description>>{
    LIVE.lock().unwrap_or_else(|e|e.into_inner()).get(&(this as usize)).cloned()
}
impl Drop for Pipeline{
    fn drop(&mut self){LIVE.lock().unwrap_or_else(|e|e.into_inner()).remove(&(std::ptr::from_ref(self) as usize));}
}
extern "system" fn query(this:This,iid:*const Iid,out:*mut This)->HRESULT{
    object::answer(PIPELINE,iid,object::is(iid,&PIPELINE_IIDS).then_some(this),out)
}
extern "system" fn release(this:This)->u32{object::release_boxed::<Pipeline>(this)}
static FUNCTIONS:Table<24>={
    let mut f=object::d3d12_object(object::missing_table::<PIPELINE,24>());
    f[0]=query as *const ();f[2]=release as *const ();f[7]=object::parent_device as *const ();Table(f)
};
fn root(this:This)->Result<Option<Arc<[u8]>>,String>{
    if this.is_null(){Ok(None)}else{crate::root::live_snapshot(this).map(Some).ok_or("invalid native root signature".into())}
}
fn create(stages:Vec<Stage>,root:Option<Arc<[u8]>>,extension_count:u32,stream:Option<Vec<u8>>,iid:*const Iid,out:*mut This)->HRESULT{
    if stages.is_empty(){return E_INVALIDARG;}
    let description=Arc::new(Description{stages,root,extension_count,stream,analysis:OnceLock::new()});
    static FIRST:std::sync::Once=std::sync::Once::new();
    FIRST.call_once(||description.analyze());
    let p=Box::new(Pipeline{object:Object::new(&FUNCTIONS),packet:[0;8],description:description.clone()});
    LIVE.lock().unwrap_or_else(|e|e.into_inner()).insert(std::ptr::from_ref(p.as_ref()) as usize,description);
    tracing::debug!(extensions=extension_count,"native guest pipeline description retained; GPU compilation pending");
    object::hand_out(PIPELINE,p,iid,&PIPELINE_IIDS,out)
}
fn failed(e:String)->HRESULT{tracing::error!(error=%e,"invalid native pipeline description");E_INVALIDARG}
pub(crate) extern "system" fn create_compute(this:This,desc:*const ComputeHead,iid:*const Iid,out:*mut This)->HRESULT{
    create_compute_x(this,desc,0,std::ptr::null(),iid,out)
}
pub(crate) extern "system" fn create_compute_x(_:This,desc:*const ComputeHead,extensions:u32,extended:*const u8,iid:*const Iid,out:*mut This)->HRESULT{
    let Some(d)=(unsafe{desc.as_ref()})else{return E_INVALIDARG;};
    if extensions>1024||(extensions!=0&&extended.is_null()){return E_INVALIDARG;}
    // Extension format is not decoded yet. Keep the count and reject use
    // until decoded; do not dereference an unknown-size extension array.
    let stages=match Stage::new("compute",&d.stage){Ok(Some(s))=>vec![s],Ok(None)=>return E_INVALIDARG,Err(e)=>return failed(e)};
    let root=match root(d.root){Ok(v)=>v,Err(e)=>return failed(e)};
    static FIRST:std::sync::Once=std::sync::Once::new();
    FIRST.call_once(||println!("VULKAN_GUEST_COMPUTE_DESCRIPTION: extensions={extensions}, shader_bytes={}, GPU pipeline pending",d.stage.size));
    create(stages,root,extensions,None,iid,out)
}
pub(crate) extern "system" fn create_graphics(_:This,desc:*const GraphicsHead,iid:*const Iid,out:*mut This)->HRESULT{
    let Some(d)=(unsafe{desc.as_ref()})else{return E_INVALIDARG;};
    let mut stages=vec![];
    for (name,code) in GRAPHICS_STAGES.iter().zip(&d.stages){
        match Stage::new(name,code){Ok(Some(s))=>stages.push(s),Ok(None)=>{},Err(e)=>return failed(e)}
    }
    let root=match root(d.root){Ok(v)=>v,Err(e)=>return failed(e)};
    // Only the legacy GraphicsHead is known, not the complete graphics state.
    create(stages,root,0,None,iid,out)
}
pub(crate) extern "system" fn create_stream(_:This,desc:*const StreamDesc,iid:*const Iid,out:*mut This)->HRESULT{
    let Some(d)=(unsafe{desc.as_ref()})else{return E_INVALIDARG;};
    let stream=match read(d.stream as usize,d.size){Ok(v)=>v,Err(e)=>return failed(e)};
    let mut at=0usize;let mut stages=vec![];let mut signature=None;
    while at<stream.len(){
        let Some(header)=stream.get(at..at.saturating_add(8))else{return E_INVALIDARG;};
        let kind=u32::from_le_bytes(header[..4].try_into().expect("four bytes"));
        let Some((size,stage))=subobject(kind)else{return failed(format!("unknown pipeline stream subobject {kind}"));};
        let Some(end)=at.checked_add(size)else{return E_INVALIDARG;};
        let Some(data)=stream.get(at..end)else{return E_INVALIDARG;};
        if let Some(name)=stage{
            let pointer=u64::from_le_bytes(data[8..16].try_into().expect("eight bytes")) as usize;
            let len=u64::from_le_bytes(data[16..24].try_into().expect("eight bytes")) as usize;
            let code=Bytecode{code:pointer as *const u8,size:len};
            match Stage::new(name,&code){Ok(Some(s))=>stages.push(s),Ok(None)=>{},Err(e)=>return failed(e)}
        }else if kind==0{
            let address=u64::from_le_bytes(data[8..16].try_into().expect("eight bytes")) as usize;
            signature=match root(address as This){Ok(v)=>v,Err(e)=>return failed(e)};
        }
        at=end;
    }
    // Never dereference pointers retained in the stream later; only copied
    // shaders and root metadata are owned at present.
    create(stages,signature,0,Some(stream),iid,out)
}

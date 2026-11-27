// SPDX-License-Identifier: GPL-3.0-or-later
//! Initial live guest GPU backend. All unsupported calls stop with a concrete
//! error instead of falling back to host D3D12 or acknowledging fake draws.
use std::sync::{Mutex,OnceLock,atomic::{AtomicBool,Ordering}};
use crate::object::{self,This};
use vortex_vulkan::live::{ImageSpec,Live,Reply,Task};
static ENABLED:AtomicBool=AtomicBool::new(false);
static WORKER:OnceLock<Result<Live,String>>=OnceLock::new();
static SUBMIT:Mutex<()>=Mutex::new(());
// CPU-side provenance only. Registered resource spans are not Vulkan GPU
// virtual addresses. Never dereference a shader-provided address here.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct Span{start:u64,end:u64}
impl Span{
    fn new(start:u64,size:u64)->Option<Self>{
        if start==0||size==0{return None;}
        Some(Self{start,end:start.checked_add(size)?})
    }
    fn contains(self,address:u64,len:u64)->bool{
        len!=0&&address>=self.start&&address.checked_add(len).is_some_and(|end|end<=self.end)
    }
}
type ResourceMap=std::collections::BTreeMap<usize,Option<Span>>;
static RESOURCES:Mutex<ResourceMap>=Mutex::new(std::collections::BTreeMap::new());

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum AddressClass{Null,Unmapped,Ambiguous,Unique{key:usize,offset:u64,remaining:u64}}
fn classify_address(resources:&ResourceMap,address:u64,len:u64)->AddressClass{
    if address==0{return AddressClass::Null;}
    let mut found=AddressClass::Unmapped;
    for (&key,span) in resources{
        let Some(span)=span else{continue;};
        if span.contains(address,len){
            if found!=AddressClass::Unmapped{return AddressClass::Ambiguous;}
            found=AddressClass::Unique{key,offset:address-span.start,remaining:span.end-address};
        }
    }
    found
}
fn root_address_status(address:u64)->(&'static str,u64){
    let resources=RESOURCES.lock().unwrap_or_else(|e|e.into_inner());
    match classify_address(&resources,address,1){
        AddressClass::Null=>("null",0),
        AddressClass::Unmapped=>("unmapped",0),
        AddressClass::Ambiguous=>("aliased",0),
        AddressClass::Unique{remaining,..}=>("registered-buffer",remaining),
    }
}
#[cfg(test)]
mod address_tests{
    use super::*;
    #[test]fn ranges_check_overflow_boundaries_and_aliasing(){
        let mut map=ResourceMap::new();
        map.insert(1,Span::new(0x1000,0x100));
        assert_eq!(classify_address(&map,0,4),AddressClass::Null);
        assert_eq!(classify_address(&map,0x10fc,4),AddressClass::Unique{key:1,offset:0xfc,remaining:4});
        assert_eq!(classify_address(&map,0x10fc,8),AddressClass::Unmapped);
        assert_eq!(classify_address(&map,0x1100,1),AddressClass::Unmapped);
        assert_eq!(Span::new(u64::MAX,2),None);
        map.insert(2,Span::new(0x1080,0x80));
        assert_eq!(classify_address(&map,0x1080,4),AddressClass::Ambiguous);
        map.remove(&2);
        assert!(matches!(classify_address(&map,0x1080,4),AddressClass::Unique{key:1,..}));
        assert_eq!(classify_address(&map,0x1080,0),AddressClass::Unmapped);
    }
}

pub fn enable(){ENABLED.store(true,Ordering::Release);}
pub fn enabled()->bool{ENABLED.load(Ordering::Acquire)}
pub(crate) fn stop(message:impl std::fmt::Display)->!{
    eprintln!("VULKAN_RUNTIME_BLOCKER: {message}; no D3D12 fallback");
    tracing::error!(%message,"VULKAN_RUNTIME_BLOCKER");
    std::process::exit(2)
}
pub(crate) fn initialize(){
    match WORKER.get_or_init(Live::start){
        Ok(_)=>{},
        Err(e)=>stop(e),
    }
    static ONCE:std::sync::Once=std::sync::Once::new();
    ONCE.call_once(||println!("VULKAN_GUEST_DEVICE_READY: real AMD device and queue"));
}
fn call(task:Task)->Reply{
    match WORKER.get().expect("guest device initialized"){
        Ok(worker)=>worker.call(task).unwrap_or_else(|e|stop(e)),
        Err(e)=>stop(e),
    }
}
pub(crate) fn register(key:usize,buffer:Option<(u64,u64)>){
    let span=buffer.and_then(|(start,size)|Span::new(start,size));
    RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).insert(key,span);
}
pub(crate) fn registered(resource:This)->bool{
    !resource.is_null()&&RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).contains_key(&(resource as usize))
}
pub(crate) fn forget(key:usize){
    RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).remove(&key);
    if matches!(WORKER.get(),Some(Ok(_))){let _=call(Task::Forget(key));}
}

pub(crate) struct Retained(usize);
impl Retained{
    pub(crate) fn new(resource:This)->Self{
        if !registered(resource){stop("invalid or retired guest resource");}
        object::add_ref(resource);Self(resource as usize)
    }
    fn get(&self)->This{self.0 as This}
}
impl Drop for Retained{
    fn drop(&mut self){crate::resource::release_live(self.get());}
}

pub(crate) enum Op{
    Sync,
    Write32{resource:Retained,offset:u64,value:u32},
    Copy{dst:Retained,src:Retained,dst_offset:u64,src_offset:u64,len:u64},
    ClearColor{resource:Retained,spec:ImageSpec,rgba:[f32;4]},
    ClearDepth{resource:Retained,spec:ImageSpec,z:f32},
    CopyImage{src:Retained,dst:Retained,from:ImageSpec,to:ImageSpec},
}
#[derive(Default)]
struct RootState{
    signature:Option<std::sync::Arc<[u8]>>,
    tables:std::collections::BTreeMap<u32,u64>,
    views:std::collections::BTreeMap<u32,(u32,u64)>,
    constants:std::collections::BTreeMap<(u32,u32),u32>,
}
#[derive(Default)]
pub(crate) struct Recording{closed:bool,ops:Vec<Op>,
    pipeline:Option<std::sync::Arc<crate::pipeline::native_live::Description>>,
    roots:[RootState;2],
    heaps:Vec<crate::descriptor::LiveHeap>}

impl Recording{
    pub(crate) fn push(&mut self,op:Op){if self.closed{stop("recording into closed guest list");}self.ops.push(op);}
    pub(crate) fn write32(&mut self,address:u64,value:u32,flush:u32,flags:u32){
        if self.closed{stop("recording write into closed guest list");}
        if flags!=0||address%4!=0{
            stop(format_args!("unsupported write32 flags or alignment: address={address:#x} flush={flush:#x} flags={flags:#x}"));
        }
        let location={
            let resources=RESOURCES.lock().unwrap_or_else(|e|e.into_inner());
            classify_address(&resources,address,4)
        };
        let AddressClass::Unique{key,offset,..}=location else{
            stop(format_args!("write32 address {address:#x} is not a unique registered buffer: {location:?} flush={flush:#x} flags={flags:#x}"));
        };
        let resource=Retained::new(key as This);
        self.push(Op::Write32{resource,offset,value});
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||tracing::info!(address,value,flush,flags,"VULKAN_GUEST_WRITE32_RECORDED"));
    }
    pub(crate) fn close(&mut self){if self.closed{stop("guest list already closed");}self.closed=true;}
    pub(crate) fn reset(&mut self){self.ops.clear();self.closed=false;self.pipeline=None;self.roots=Default::default();self.heaps.clear();}
    pub(crate) fn set_pipeline(&mut self,pipeline:This){
        if self.closed{stop("binding into closed guest list");}
        self.pipeline=if pipeline.is_null(){None}else{Some(crate::pipeline::native_live::get(pipeline).unwrap_or_else(||stop("invalid native pipeline object")))};
    }
    pub(crate) fn set_heaps(&mut self,heaps:&[This]){
        if self.closed||heaps.len()>2{stop("invalid descriptor heap binding");}
        let mut retained: Vec<crate::descriptor::LiveHeap> = Vec::new();
        for &heap in heaps{
            let h=crate::descriptor::LiveHeap::retain(heap).unwrap_or_else(||stop("unknown or non-shader-visible descriptor heap"));
            if retained.iter().any(|old:&crate::descriptor::LiveHeap|old.kind==h.kind){stop("duplicate descriptor heap type");}
            retained.push(h);
        }
        retained.sort_by_key(|h|h.kind);
        if self.heaps!=retained{
            self.heaps=retained;
            for root in &mut self.roots{root.tables.clear();}
        }
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||tracing::info!(heaps=?self.heaps,"VULKAN_GUEST_DESCRIPTOR_HEAPS_BOUND"));
    }
    fn root_state(&mut self,compute:bool)->&mut RootState{
        if self.closed{stop("setting root state in closed list");}
        &mut self.roots[usize::from(compute)]
    }
    pub(crate) fn set_root(&mut self,compute:bool,root:This){
        let signature=if root.is_null(){None}else{Some(crate::root::live_snapshot(root).unwrap_or_else(||stop("unknown bound root signature")))};
        let state=self.root_state(compute);
        // A different signature invalidates previous root bindings.
        let unchanged=match (&state.signature,&signature){
            (Some(a),Some(b))=>std::sync::Arc::ptr_eq(a,b),(None,None)=>true,_=>false
        };
        if !unchanged{*state=RootState{signature,..Default::default()};}
    }
    pub(crate) fn set_table(&mut self,compute:bool,slot:u32,handle:u64){
        if slot>=1024{stop("invalid root table slot");}
        let state=self.root_state(compute);
        state.views.remove(&slot);state.tables.insert(slot,handle);
        state.constants.retain(|&(s,_),_|s!=slot);
    }
    pub(crate) fn set_view(&mut self,compute:bool,slot:u32,kind:u32,address:u64){
        if slot>=1024{stop("invalid root view slot");}
        let state=self.root_state(compute);
        state.tables.remove(&slot);state.views.insert(slot,(kind,address));
        state.constants.retain(|&(s,_),_|s!=slot);
    }
    pub(crate) fn set_constants(&mut self,compute:bool,slot:u32,offset:u32,values:&[u32]){
        if slot>=1024||offset.checked_add(values.len() as u32).is_none_or(|n|n>64){stop("invalid root constants range");}
        let state=self.root_state(compute);
        state.tables.remove(&slot);state.views.remove(&slot);
        for (i,&value) in values.iter().enumerate(){state.constants.insert((slot,offset+i as u32),value);}
    }
    pub(crate) fn require_pipeline(&self,kind:&str)->!{
        let root=&self.roots[usize::from(kind=="dispatch")];
        tracing::info!(kind,heaps=?self.heaps,table_handles_valid=root.tables.values().all(|&h|self.heaps.iter().any(|heap|heap.contains(h))),root_bytes=root.signature.as_ref().map_or(0,|v|v.len()),
            table_slots=?root.tables.keys().collect::<Vec<_>>(),
            view_slots=?root.views.iter().map(|(k,v)|(*k,v.0,root_address_status(v.1))).collect::<Vec<_>>(),
            constant_dwords=root.constants.len(),"VULKAN_GUEST_ROOT_BINDINGS");
        self.pipeline.as_ref().unwrap_or_else(||stop(format_args!("{kind}: no bound native pipeline"))).require_execution(kind)
    }
    pub(crate) fn execute(&self){
        if !self.closed{stop("submitting open guest list");}
        for op in &self.ops{
            match op{
                Op::Sync=>{
                    let _=call(Task::Sync);
                    static FIRST:std::sync::Once=std::sync::Once::new();
                    FIRST.call_once(||println!("VULKAN_GUEST_SYNC_OK: native synchronization2 barrier submitted and completed"));
                }
                Op::Write32{resource,offset,value}=>{
                    // Current live backend completes each submitted operation
                    // synchronously; widen the guest flush to a full GPU sync.
                    let _=call(Task::Sync);
                    let address=range(resource.get(),*offset,4);
                    let bytes=value.to_le_bytes();
                    let mut actual=0usize;
                    use windows_sys::Win32::System::{Diagnostics::Debug::WriteProcessMemory,Threading::GetCurrentProcess};
                    if unsafe{WriteProcessMemory(GetCurrentProcess(),address as *mut _,bytes.as_ptr().cast(),bytes.len(),&mut actual)}==0||actual!=bytes.len(){
                        stop("guest write32 destination is not writable");
                    }
                    static FIRST:std::sync::Once=std::sync::Once::new();
                    FIRST.call_once(||println!("VULKAN_GUEST_WRITE32_OK: ordered write to registered guest buffer"));
                }
                Op::Copy{dst,src,dst_offset,src_offset,len}=>copy(dst.get(),src.get(),*dst_offset,*src_offset,*len),
                Op::ClearColor{resource,spec,rgba}=>{
                    if resource.0!=spec.key{stop("color resource identity mismatch");}
                    let _=call(Task::ClearColor{image:*spec,rgba:*rgba});
                }
                Op::ClearDepth{resource,spec,z}=>{
                    if resource.0!=spec.key{stop("depth resource identity mismatch");}
                    let _=call(Task::ClearDepth{image:*spec,z:*z});
                }
                Op::CopyImage{src,dst,from,to}=>{
                    if src.0!=from.key||dst.0!=to.key{stop("image copy identity mismatch");}
                    let _=call(Task::CopyImage{src:*from,dst:*to});
                }
            }
        }
    }
}
pub(crate) fn submit_lock()->std::sync::MutexGuard<'static,()>{
    SUBMIT.lock().unwrap_or_else(|e|e.into_inner())
}
fn range(resource:This,offset:u64,len:u64)->usize{
    if !registered(resource){stop("invalid guest buffer");}
    let(data,size)=crate::resource::buffer_bytes(resource).unwrap_or_else(||stop("copy requires guest buffer"));
    if offset.checked_add(len).is_none_or(|end|end>size){stop("guest buffer copy out of range");}
    let offset=usize::try_from(offset).unwrap_or_else(|_|stop("guest offset exceeds address space"));
    (data as usize).checked_add(offset).unwrap_or_else(||stop("guest address overflow"))
}
fn copy(dst:This,src:This,dst_offset:u64,src_offset:u64,len:u64){
    use windows_sys::Win32::System::{Diagnostics::Debug::{ReadProcessMemory,WriteProcessMemory},Threading::GetCurrentProcess};
    let from=range(src,src_offset,len);let to=range(dst,dst_offset,len);
    let n=usize::try_from(len).unwrap_or_else(|_|stop("copy size exceeds address space"));
    let end=from.checked_add(n).unwrap_or_else(||stop("source overflow"));
    let _=to.checked_add(n).unwrap_or_else(||stop("destination overflow"));
    // Copy in the correct direction even for overlapping guest addresses.
    let backward=to>from&&to<end;
    let mut done=0usize;
    while done<n{
        let size=(n-done).min(4*1024*1024);
        let offset=if backward{n-done-size}else{done};
        let mut bytes=vec![0u8;size];let mut actual=0usize;
        // SAFETY: own process; kernel verifies the guest mapping; exact byte
        // counts are required. The worker sees owned bytes, not guest pointers.
        if unsafe{ReadProcessMemory(GetCurrentProcess(),(from+offset) as *const _,bytes.as_mut_ptr().cast(),size,&mut actual)}==0||actual!=size{
            stop("guest buffer source is not readable");
        }
        let Reply::Bytes(bytes)=call(Task::CopyBytes(bytes))else{stop("invalid Vulkan copy response");};
        if bytes.len()!=size{stop("invalid Vulkan readback length");}
        actual=0;
        if unsafe{WriteProcessMemory(GetCurrentProcess(),(to+offset) as *mut _,bytes.as_ptr().cast(),size,&mut actual)}==0||actual!=size{
            stop("guest buffer destination is not writable");
        }
        done+=size;
    }
    if n>0{
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||println!("VULKAN_GUEST_COPY_OK: {n} actual guest bytes transferred through GPU"));
    }
}
pub(crate) fn present(resource:This){
    let _lock=submit_lock();
    let hold=Retained::new(resource);
    let spec=crate::resource::live_image_spec(hold.get()).unwrap_or_else(||stop("unsupported guest present resource"));
    if spec.depth{stop("depth image is not a presentation source");}
    static WINDOW:OnceLock<usize>=OnceLock::new();
    let hwnd=*WINDOW.get_or_init(||{
        let window=crate::host::title_window().or_else(||crate::host::window(spec.width,spec.height).ok())
            .unwrap_or_else(||stop("cannot create live presentation window"));
        window.0 as usize
    });
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hinstance=unsafe{GetModuleHandleW(None)}.unwrap_or_else(|e|stop(e)).0 as usize;
    let _=call(Task::Present{image:spec,hwnd,hinstance});
    static FIRST:std::sync::Once=std::sync::Once::new();
    FIRST.call_once(||println!("FIRST_VULKAN_GUEST_PRESENT_OK: {}x{} actual guest target (not proof of scene rendering)",spec.width,spec.height));
}
